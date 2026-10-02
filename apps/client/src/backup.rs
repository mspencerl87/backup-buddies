use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use age::secrecy::SecretString;
use anyhow::{Context, Result};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl};
use sha2::{Digest, Sha256};

use crate::manifest::{Manifest, ManifestEntry};
use crate::netinfo::{self, ConnectionQuality};
use crate::protocol::{self, SyncAck, SyncRequest};

/// What one backup cycle actually did, including transfer metrics — not
/// just file counts. `bytes_sent`/`duration_ms` come straight off the
/// connection's own QUIC stats (so they include protocol overhead, not
/// just ciphertext — a more honest "how long did this take" number than
/// summing file sizes), letting the dashboard show a real transfer rate
/// for the cycle instead of just "sent 3 files". Zeroed/None when the
/// cycle found nothing to do and never opened a connection at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct BackupCycleStats {
    pub files_sent: usize,
    pub files_deleted: usize,
    pub bytes_sent: u64,
    pub duration_ms: u64,
    pub connection: Option<ConnectionQuality>,
}

/// One pass: scan `backup_dir`, diff against what we last told `buddy_id`
/// we'd sent them, and push anything new, changed, or deleted. Connects to
/// the buddy only if there's actually something to send — most cycles,
/// after the first, should find nothing changed and do no network work at
/// all.
pub async fn run_backup_cycle(
    endpoint: &Endpoint,
    relay_url: &RelayUrl,
    passphrase: &SecretString,
    data_dir: &Path,
    backup_dir: &Path,
    buddy_id: EndpointId,
) -> Result<BackupCycleStats> {
    let buddy_node_id = buddy_id.to_string();
    let mut manifest = Manifest::load(data_dir, &buddy_node_id).await;

    let local_files = scan_local_files(backup_dir).await?;

    let mut to_put: Vec<(String, PathBuf, u64, String)> = Vec::new();
    for (rel_path, abs_path, size) in &local_files {
        let sha256 = hash_file(abs_path).await?;
        let changed = manifest
            .entries
            .get(rel_path)
            .map(|e| e.sha256 != sha256 || e.size != *size)
            .unwrap_or(true);
        if changed {
            to_put.push((rel_path.clone(), abs_path.clone(), *size, sha256));
        }
    }

    let local_paths: HashSet<&String> = local_files.iter().map(|(rel, _, _)| rel).collect();
    let to_delete: Vec<String> =
        manifest.entries.keys().filter(|rel| !local_paths.contains(rel)).cloned().collect();

    if to_put.is_empty() && to_delete.is_empty() {
        return Ok(BackupCycleStats::default());
    }

    let cycle_started = std::time::Instant::now();
    let addr = EndpointAddr::new(buddy_id).with_relay_url(relay_url.clone());
    let conn = endpoint
        .connect(addr, protocol::ALPN)
        .await
        .context("failed to connect to buddy for backup")?;

    let recipient = age::scrypt::Recipient::new(passphrase.clone());

    let mut put_count = 0;
    for (rel_path, abs_path, size, sha256) in &to_put {
        match put_one_file(&conn, &recipient, rel_path, abs_path).await {
            Ok(ciphertext_size) => {
                manifest.entries.insert(
                    rel_path.clone(),
                    ManifestEntry { size: *size, sha256: sha256.clone(), ciphertext_size },
                );
                put_count += 1;
            }
            Err(err) => {
                tracing::warn!(path = %rel_path, ?err, "failed to back up file — will retry next cycle");
            }
        }
    }

    let mut delete_count = 0;
    for rel_path in &to_delete {
        match delete_one_file(&conn, rel_path).await {
            Ok(()) => {
                manifest.entries.remove(rel_path);
                delete_count += 1;
            }
            Err(err) => {
                tracing::warn!(path = %rel_path, ?err, "failed to tell buddy about a deleted file — will retry next cycle");
            }
        }
    }

    // Save whatever progress we made even if some files above failed —
    // partial progress shouldn't be lost, and the ones that failed simply
    // stay in the "changed" diff next cycle since the manifest wasn't
    // updated for them.
    manifest.save(data_dir, &buddy_node_id).await?;

    // Read straight off the connection's own QUIC stats rather than
    // summing what we think we sent — covers protocol overhead (framing,
    // acks, retransmits) too, which is what actually took the time.
    let stats = BackupCycleStats {
        files_sent: put_count,
        files_deleted: delete_count,
        bytes_sent: conn.stats().udp_tx.bytes,
        duration_ms: cycle_started.elapsed().as_millis() as u64,
        connection: Some(netinfo::connection_quality(&conn)),
    };
    Ok(stats)
}

// Returns the ciphertext size on success — what actually got sent over
// the wire and stored on the buddy's end, which is what the caller
// records in the manifest for the dashboard to show (see ManifestEntry's
// ciphertext_size field for why that's not just `size`).
async fn put_one_file(
    conn: &iroh::endpoint::Connection,
    recipient: &age::scrypt::Recipient,
    rel_path: &str,
    abs_path: &Path,
) -> Result<u64> {
    let plaintext = tokio::fs::read(abs_path).await.context("failed to read local file")?;
    let ciphertext = age::encrypt(recipient, &plaintext).context("failed to encrypt file")?;
    let ciphertext_sha256 = hex::encode(Sha256::digest(&ciphertext));
    let ciphertext_len = ciphertext.len() as u64;

    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(
        &mut send,
        &SyncRequest::Put {
            path: rel_path.to_string(),
            ciphertext_len,
            ciphertext_sha256,
        },
    )
    .await?;
    send.write_all(&ciphertext).await.context("failed to send file bytes")?;
    send.finish().context("failed to finish send stream")?;

    let ack: SyncAck = protocol::read_frame(&mut recv).await.context("failed to read ack")?;
    if !ack.ok {
        anyhow::bail!("buddy rejected file: {}", ack.error.unwrap_or_default());
    }
    Ok(ciphertext_len)
}

async fn delete_one_file(conn: &iroh::endpoint::Connection, rel_path: &str) -> Result<()> {
    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(&mut send, &SyncRequest::Delete { path: rel_path.to_string() }).await?;
    send.finish().context("failed to finish send stream")?;

    let ack: SyncAck = protocol::read_frame(&mut recv).await.context("failed to read ack")?;
    if !ack.ok {
        anyhow::bail!("buddy rejected delete: {}", ack.error.unwrap_or_default());
    }
    Ok(())
}

async fn hash_file(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path).await.context("failed to read file for hashing")?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

// walkdir is synchronous; fine for now given the modest directory sizes
// this is built for, but a large BACKUP_DIR would block the async runtime
// for the duration of the scan. If that becomes a problem, wrap this body
// in tokio::task::spawn_blocking.
async fn scan_local_files(backup_dir: &Path) -> Result<Vec<(String, PathBuf, u64)>> {
    let mut out = Vec::new();
    for entry in
        walkdir::WalkDir::new(backup_dir).follow_links(false).into_iter().filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let abs_path = entry.path().to_path_buf();
        let rel_path = abs_path
            .strip_prefix(backup_dir)
            .unwrap_or(&abs_path)
            .to_string_lossy()
            .replace('\\', "/"); // normalize separators in case this ever runs on Windows
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push((rel_path, abs_path, size));
    }
    Ok(out)
}
