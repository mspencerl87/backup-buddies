use std::path::Path;

use age::secrecy::SecretString;
use anyhow::{Context, Result};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl};

use crate::protocol::{self, ListResponse, SyncAck, SyncRequest};
use crate::receive::safe_join;

const MAX_FILE_BYTES: usize = 5 * 1024 * 1024 * 1024; // matches receive.rs's per-file cap

/// Pulls everything a buddy is holding for us back down, decrypts it with
/// our own passphrase, and writes it under `restore_dir` (mirroring the
/// relative paths it was backed up with). Meant for manual, one-shot use —
/// see the `restore` subcommand in main.rs — not run on the periodic loop.
/// Returns the number of files successfully restored.
pub async fn run_restore(
    endpoint: &Endpoint,
    relay_url: &RelayUrl,
    passphrase: &SecretString,
    buddy_id: EndpointId,
    restore_dir: &Path,
) -> Result<usize> {
    let addr = EndpointAddr::new(buddy_id).with_relay_url(relay_url.clone());
    let conn = endpoint
        .connect(addr, protocol::ALPN)
        .await
        .context("failed to connect to buddy for restore")?;

    let files = {
        let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
        protocol::write_frame(&mut send, &SyncRequest::List).await?;
        send.finish().context("failed to finish send stream")?;
        let resp: ListResponse =
            protocol::read_frame(&mut recv).await.context("failed to read file list")?;
        resp.files
    };

    tracing::info!(count = files.len(), "buddy reports this many files on restore");

    let identity = age::scrypt::Identity::new(passphrase.clone());

    let mut restored = 0;
    for entry in &files {
        match restore_one_file(&conn, &identity, restore_dir, &entry.path).await {
            Ok(()) => restored += 1,
            Err(err) => tracing::warn!(path = %entry.path, ?err, "failed to restore file"),
        }
    }

    Ok(restored)
}

async fn restore_one_file(
    conn: &iroh::endpoint::Connection,
    identity: &age::scrypt::Identity,
    restore_dir: &Path,
    rel_path: &str,
) -> Result<()> {
    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(&mut send, &SyncRequest::Get { path: rel_path.to_string() }).await?;
    send.finish().context("failed to finish send stream")?;

    let ack: SyncAck = protocol::read_frame(&mut recv).await.context("failed to read ack")?;
    if !ack.ok {
        anyhow::bail!("buddy could not serve file: {}", ack.error.unwrap_or_default());
    }

    let ciphertext =
        recv.read_to_end(MAX_FILE_BYTES).await.context("failed to read file bytes")?;
    let plaintext =
        age::decrypt(identity, &ciphertext).context("failed to decrypt file — wrong passphrase?")?;

    // rel_path came from the buddy's List response — same guard as the
    // receive side, since there's no reason to trust it more just because
    // it round-tripped through a buddy instead of arriving directly.
    let Some(dest) = safe_join(restore_dir, rel_path) else {
        anyhow::bail!("buddy sent an unsafe path in its file list: {rel_path:?}");
    };
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await.context("failed to create restore directory")?;
    }
    tokio::fs::write(&dest, &plaintext).await.context("failed to write restored file")?;
    Ok(())
}
