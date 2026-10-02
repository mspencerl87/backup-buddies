use std::path::Path;

use age::secrecy::SecretString;
use anyhow::{Context, Result};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl};

use crate::netinfo::{self, ConnectionQuality};
use crate::protocol::{
    self, DiskStatsResponse, ListEntry, ListResponse, ListVersionsResponse, SyncAck, SyncRequest,
    VersionEntry,
};
use crate::receive::safe_join;

const MAX_FILE_BYTES: usize = 5 * 1024 * 1024 * 1024; // matches receive.rs's per-file cap

/// What one restore actually did, including transfer metrics — mirrors
/// backup.rs's BackupCycleStats so the dashboard can show both directions
/// the same way. bytes_restored sums each file's *ciphertext* length (the
/// decrypted plaintext is what lands on disk, but ciphertext is what
/// crossed the network, which is what a transfer rate should be measured
/// against).
#[derive(Debug, Clone, Copy, Default)]
pub struct RestoreStats {
    pub files_restored: usize,
    pub bytes_restored: u64,
    pub duration_ms: u64,
    pub connection: Option<ConnectionQuality>,
}

/// Connects to a buddy and asks what they're holding for us — no
/// decryption, no writing to disk, just the file list (path + ciphertext
/// size). Used by the dashboard's file browser to show what's available
/// before the person picks what to restore, and by `run_restore` below to
/// get the full list it then restores everything from.
pub async fn list_buddy_files(
    endpoint: &Endpoint,
    relay_url: &RelayUrl,
    buddy_id: EndpointId,
) -> Result<Vec<ListEntry>> {
    let conn = connect(endpoint, relay_url, buddy_id).await?;
    list_on(&conn).await
}

/// Connects to a buddy and asks for their real disk numbers (see
/// DiskStatsResponse), plus our own local read on the quality of that
/// connection (direct vs. relayed, round-trip latency) — used by the
/// dashboard to show what a buddy actually has available, next to what
/// they've pledged, so an over-pledged or nearly-full buddy is visible
/// without either side having to ask the other directly.
pub async fn fetch_buddy_disk_stats(
    endpoint: &Endpoint,
    relay_url: &RelayUrl,
    buddy_id: EndpointId,
) -> Result<(DiskStatsResponse, ConnectionQuality)> {
    let conn = connect(endpoint, relay_url, buddy_id).await?;
    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(&mut send, &SyncRequest::DiskStats).await?;
    send.finish().context("failed to finish send stream")?;
    let stats: DiskStatsResponse =
        protocol::read_frame(&mut recv).await.context("failed to read disk stats")?;
    Ok((stats, netinfo::connection_quality(&conn)))
}

/// Asks a buddy what backup slots still exist for one path — the file
/// browser's "History" affordance uses this to show what's recoverable
/// before the person picks a version to restore.
pub async fn list_file_versions(
    endpoint: &Endpoint,
    relay_url: &RelayUrl,
    buddy_id: EndpointId,
    path: &str,
) -> Result<Vec<VersionEntry>> {
    let conn = connect(endpoint, relay_url, buddy_id).await?;
    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(&mut send, &SyncRequest::ListVersions { path: path.to_string() }).await?;
    send.finish().context("failed to finish send stream")?;
    let resp: ListVersionsResponse =
        protocol::read_frame(&mut recv).await.context("failed to read version list")?;
    Ok(resp.versions)
}

/// Restores one specific backup slot of one file — not the live copy.
/// Written alongside the live path with a suffix (`<path>.v<N>.bak`) so it
/// never clobbers a normal restore of the same file.
pub async fn restore_version(
    endpoint: &Endpoint,
    relay_url: &RelayUrl,
    passphrase: &SecretString,
    buddy_id: EndpointId,
    restore_dir: &Path,
    path: &str,
    version: u32,
) -> Result<RestoreStats> {
    let started = std::time::Instant::now();
    let conn = connect(endpoint, relay_url, buddy_id).await?;
    let identity = age::scrypt::Identity::new(passphrase.clone());

    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(&mut send, &SyncRequest::GetVersion { path: path.to_string(), version }).await?;
    send.finish().context("failed to finish send stream")?;

    let ack: SyncAck = protocol::read_frame(&mut recv).await.context("failed to read ack")?;
    if !ack.ok {
        anyhow::bail!("buddy could not serve that version: {}", ack.error.unwrap_or_default());
    }
    let ciphertext = recv.read_to_end(MAX_FILE_BYTES).await.context("failed to read file bytes")?;
    let ciphertext_len = ciphertext.len() as u64;
    let plaintext =
        age::decrypt(&identity, &ciphertext).context("failed to decrypt file — wrong passphrase?")?;

    let suffixed = format!("{path}.v{version}.bak");
    let Some(dest) = safe_join(restore_dir, &suffixed) else {
        anyhow::bail!("buddy sent an unsafe path: {path:?}");
    };
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await.context("failed to create restore directory")?;
    }
    tokio::fs::write(&dest, &plaintext).await.context("failed to write restored file")?;

    Ok(RestoreStats {
        files_restored: 1,
        bytes_restored: ciphertext_len,
        duration_ms: started.elapsed().as_millis() as u64,
        connection: Some(netinfo::connection_quality(&conn)),
    })
}

async fn connect(
    endpoint: &Endpoint,
    relay_url: &RelayUrl,
    buddy_id: EndpointId,
) -> Result<iroh::endpoint::Connection> {
    let addr = EndpointAddr::new(buddy_id).with_relay_url(relay_url.clone());
    endpoint.connect(addr, protocol::ALPN).await.context("failed to connect to buddy")
}

async fn list_on(conn: &iroh::endpoint::Connection) -> Result<Vec<ListEntry>> {
    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(&mut send, &SyncRequest::List).await?;
    send.finish().context("failed to finish send stream")?;
    let resp: ListResponse =
        protocol::read_frame(&mut recv).await.context("failed to read file list")?;
    Ok(resp.files)
}

/// Pulls everything a buddy is holding for us back down, decrypts it with
/// our own passphrase, and writes it under `restore_dir` (mirroring the
/// relative paths it was backed up with). Meant for manual, one-shot use —
/// the `restore` CLI subcommand, or the dashboard's "Restore everything"
/// button.
pub async fn run_restore(
    endpoint: &Endpoint,
    relay_url: &RelayUrl,
    passphrase: &SecretString,
    buddy_id: EndpointId,
    restore_dir: &Path,
) -> Result<RestoreStats> {
    let started = std::time::Instant::now();
    let conn = connect(endpoint, relay_url, buddy_id).await?;
    let files = list_on(&conn).await?;
    tracing::info!(count = files.len(), "buddy reports this many files on restore");

    let paths: Vec<String> = files.into_iter().map(|f| f.path).collect();
    let (files_restored, bytes_restored) =
        restore_paths_on(&conn, passphrase, restore_dir, &paths).await?;
    Ok(RestoreStats {
        files_restored,
        bytes_restored,
        duration_ms: started.elapsed().as_millis() as u64,
        connection: Some(netinfo::connection_quality(&conn)),
    })
}

/// Same as `run_restore`, but only the given paths (as returned by
/// `list_buddy_files`) rather than everything — what the dashboard's file
/// browser uses once the person has picked specific files. Connects once
/// and restores every requested path over that one connection rather than
/// reconnecting per file.
pub async fn restore_selected(
    endpoint: &Endpoint,
    relay_url: &RelayUrl,
    passphrase: &SecretString,
    buddy_id: EndpointId,
    restore_dir: &Path,
    paths: &[String],
) -> Result<RestoreStats> {
    let started = std::time::Instant::now();
    let conn = connect(endpoint, relay_url, buddy_id).await?;
    let (files_restored, bytes_restored) =
        restore_paths_on(&conn, passphrase, restore_dir, paths).await?;
    Ok(RestoreStats {
        files_restored,
        bytes_restored,
        duration_ms: started.elapsed().as_millis() as u64,
        connection: Some(netinfo::connection_quality(&conn)),
    })
}

// Returns (files restored, total ciphertext bytes read) — the byte total
// is what a transfer rate should be computed against, same reasoning as
// backup.rs's put_one_file returning ciphertext size rather than
// plaintext size.
async fn restore_paths_on(
    conn: &iroh::endpoint::Connection,
    passphrase: &SecretString,
    restore_dir: &Path,
    paths: &[String],
) -> Result<(usize, u64)> {
    let identity = age::scrypt::Identity::new(passphrase.clone());

    let mut restored = 0;
    let mut bytes = 0u64;
    for rel_path in paths {
        match restore_one_file(conn, &identity, restore_dir, rel_path).await {
            Ok(ciphertext_len) => {
                restored += 1;
                bytes += ciphertext_len;
            }
            Err(err) => tracing::warn!(path = %rel_path, ?err, "failed to restore file"),
        }
    }
    Ok((restored, bytes))
}

// Returns the ciphertext length read off the wire on success — see
// restore_paths_on's doc comment for why the caller wants that, not the
// decrypted plaintext length.
async fn restore_one_file(
    conn: &iroh::endpoint::Connection,
    identity: &age::scrypt::Identity,
    restore_dir: &Path,
    rel_path: &str,
) -> Result<u64> {
    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(&mut send, &SyncRequest::Get { path: rel_path.to_string() }).await?;
    send.finish().context("failed to finish send stream")?;

    let ack: SyncAck = protocol::read_frame(&mut recv).await.context("failed to read ack")?;
    if !ack.ok {
        anyhow::bail!("buddy could not serve file: {}", ack.error.unwrap_or_default());
    }

    let ciphertext =
        recv.read_to_end(MAX_FILE_BYTES).await.context("failed to read file bytes")?;
    let ciphertext_len = ciphertext.len() as u64;
    let plaintext =
        age::decrypt(identity, &ciphertext).context("failed to decrypt file — wrong passphrase?")?;

    // rel_path came from the buddy's List response (or was echoed back
    // from the dashboard's own browser, which only ever got it from that
    // same List response) — same guard as the receive side either way,
    // since there's no reason to trust it more just because it
    // round-tripped through a buddy instead of arriving directly.
    let Some(dest) = safe_join(restore_dir, rel_path) else {
        anyhow::bail!("buddy sent an unsafe path in its file list: {rel_path:?}");
    };
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await.context("failed to create restore directory")?;
    }
    tokio::fs::write(&dest, &plaintext).await.context("failed to write restored file")?;
    Ok(ciphertext_len)
}
