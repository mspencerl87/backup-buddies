use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use age::secrecy::SecretString;
use anyhow::{Context, Result};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl};
use sha2::{Digest, Sha256};

use crate::manifest::{Manifest, ManifestEntry};
use crate::netinfo::{self, ConnectionQuality};
use crate::protocol::{self, PutRejectReason, SyncAck, SyncRequest};

/// One path that failed to back up this cycle, with a human-readable
/// reason when we have one. `reason` is `Some` only when the buddy told us
/// *why* via a STOP_SENDING code (see protocol::PutRejectReason) — any
/// other failure (connection drop, local read error, a delete that
/// failed) still shows up here, just without a specific explanation
/// beyond "failed to sync", since those aren't the buddy actively
/// rejecting the file.
#[derive(Debug, Clone)]
pub struct FailedFile {
    pub path: String,
    pub reason: Option<&'static str>,
}

/// What one backup cycle actually did, including transfer metrics — not
/// just file counts. `bytes_sent`/`duration_ms` come straight off the
/// connection's own QUIC stats (so they include protocol overhead, not
/// just ciphertext — a more honest "how long did this take" number than
/// summing file sizes), letting the dashboard show a real transfer rate
/// for the cycle instead of just "sent 3 files". Zeroed/None when the
/// cycle found nothing to do and never opened a connection at all.
#[derive(Debug, Clone, Default)]
pub struct BackupCycleStats {
    pub files_sent: usize,
    pub files_deleted: usize,
    pub bytes_sent: u64,
    pub duration_ms: u64,
    pub connection: Option<ConnectionQuality>,
    // How many put/delete calls failed this cycle — not fatal (the cycle
    // itself still ran, and anything that failed just stays in next
    // cycle's diff and gets retried), but worth the dashboard calling out:
    // a file that keeps failing every cycle isn't a transient blip.
    pub files_failed: usize,
    // A capped sample of which paths failed, with a reason when the buddy
    // gave us one — not every failure if there are many, just enough to
    // point someone at the problem.
    pub failed_paths: Vec<FailedFile>,
}

const MAX_FAILED_PATHS_SHOWN: usize = 10;

/// Pulls a `PutRejectReason` out of a `put_one_file` error, when the
/// failure was the buddy's own STOP_SENDING telling us why — walks the
/// anyhow error's source chain since `write_all`'s `WriteError::Stopped`
/// gets wrapped in `.context(...)` on the way out.
fn reject_reason_from_err(err: &anyhow::Error) -> Option<&'static str> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<iroh::endpoint::WriteError>())
        .and_then(|werr| match werr {
            iroh::endpoint::WriteError::Stopped(code) => {
                PutRejectReason::from_code(code.into_inner())
            }
            _ => None,
        })
        .map(PutRejectReason::message)
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
    let mut files_failed = 0usize;
    let mut failed_paths = Vec::new();
    for (rel_path, abs_path, size, sha256) in &to_put {
        match put_one_file(&conn, &recipient, rel_path, abs_path).await {
            Ok(ciphertext_size) => {
                manifest.entries.insert(
                    rel_path.clone(),
                    ManifestEntry { size: *size, sha256: sha256.clone(), ciphertext_size },
                );
                put_count += 1;
                // Persist right away rather than waiting for the whole
                // cycle to finish — a cycle can be sending many files, and
                // if this process gets killed partway through (a restart,
                // an OOM, or a buddy blocking the connection on a later
                // file), only saving once at the end would lose every
                // already-landed file from *this* cycle, not just the one
                // in flight when it died. The buddy's own put handling is
                // idempotent, so a lost manifest update only costs a
                // redundant re-send next cycle rather than corrupting
                // anything — but there's no reason to pay that cost for
                // more files than necessary. (This is exactly what
                // happened to pledge-test-2/4/6 during the deadlock-bug
                // investigation: the cycle got killed by a manual restart
                // while a later file in the same cycle was stuck, and the
                // single end-of-cycle save never ran.)
                if let Err(err) = manifest.save(data_dir, &buddy_node_id).await {
                    tracing::warn!(
                        path = %rel_path, ?err,
                        "backed up file but failed to persist the manifest update — may be re-sent next cycle"
                    );
                }
            }
            Err(err) => {
                let reason = reject_reason_from_err(&err);
                tracing::warn!(path = %rel_path, ?err, reason, "failed to back up file — will retry next cycle");
                files_failed += 1;
                if failed_paths.len() < MAX_FAILED_PATHS_SHOWN {
                    failed_paths.push(FailedFile { path: rel_path.clone(), reason });
                }
            }
        }
    }

    let mut delete_count = 0;
    for rel_path in &to_delete {
        match delete_one_file(&conn, rel_path).await {
            Ok(()) => {
                manifest.entries.remove(rel_path);
                delete_count += 1;
                // Same reasoning as the put loop above — persist each
                // deletion as it's confirmed, not just once at the end.
                if let Err(err) = manifest.save(data_dir, &buddy_node_id).await {
                    tracing::warn!(
                        path = %rel_path, ?err,
                        "told buddy about a deleted file but failed to persist the manifest update — may retry next cycle"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(path = %rel_path, ?err, "failed to tell buddy about a deleted file — will retry next cycle");
                files_failed += 1;
                if failed_paths.len() < MAX_FAILED_PATHS_SHOWN {
                    failed_paths.push(FailedFile { path: rel_path.clone(), reason: None });
                }
            }
        }
    }

    // One more save as a backstop — harmless (the in-memory state already
    // matches disk in the common case, since every change above just
    // persisted itself), but cheap insurance against a transient write
    // error above getting silently left unresolved for an entire cycle.
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
        files_failed,
        failed_paths,
    };
    Ok(stats)
}

/// Result of comparing our manifest against what the buddy actually
/// reports holding for us — see `reconcile_with_buddy` below.
#[derive(Debug, Clone, Default)]
pub struct ReconcileStats {
    /// How many manifest entries were checked against the buddy's list.
    pub checked: usize,
    /// Paths our manifest believes landed, but the buddy either doesn't
    /// have (at all, or only as a `.versions/` backup slot — see
    /// `ListEntry::deleted`) or has at a different size. Already dropped
    /// from the manifest by the time this is returned, so the very next
    /// ordinary cycle re-sends them automatically — no separate re-send
    /// logic needed here.
    pub missing_on_buddy: Vec<String>,
    /// Paths the buddy has (live, not a `.versions/`-only slot) that our
    /// manifest has no record of sending. Reported only, never acted on
    /// automatically — deleting these from the buddy based on a guess
    /// would be destructive if the guess is wrong (e.g. a manifest this
    /// process just hasn't caught up on yet), so this is purely a loud
    /// surface for a person to look into.
    pub unexpected_on_buddy: Vec<String>,
}

/// Checks that the buddy's own actual stored files still match what our
/// manifest believes we sent them — unlike `run_backup_cycle` above,
/// which only ever diffs *local* files against the manifest (what we
/// believe we already sent), this catches drift that happens entirely
/// outside the normal put/delete flow: manual filesystem surgery on the
/// receiver, a bug, partial data loss on their end, restoring from an
/// old snapshot. None of those change anything about our local source
/// files, so an ordinary cycle has no way to ever notice them on its
/// own — this is what does. Meant to be called periodically rather than
/// every cycle (see main.rs's reconcile interval) since it costs the
/// buddy a full `List` of everything they're holding for us, heavier
/// than the ordinary diff-driven cycle which only touches the network
/// when something local actually changed.
pub async fn reconcile_with_buddy(
    endpoint: &Endpoint,
    relay_url: &RelayUrl,
    data_dir: &Path,
    buddy_id: EndpointId,
) -> Result<ReconcileStats> {
    let buddy_node_id = buddy_id.to_string();
    let mut manifest = Manifest::load(data_dir, &buddy_node_id).await;
    if manifest.entries.is_empty() {
        // Nothing we believe we've sent this buddy, so nothing to check
        // — and no reason to open a connection just to confirm that.
        return Ok(ReconcileStats::default());
    }

    let buddy_files = crate::restore::list_buddy_files(endpoint, relay_url, buddy_id)
        .await
        .context("failed to list buddy's files for reconciliation")?;

    // Only live files count as "the buddy still has this" — a
    // `deleted: true` entry is a `.versions/` backup slot from something
    // that was overwritten or deleted, not the live file our manifest is
    // tracking (see ListEntry's doc comment in protocol.rs).
    let buddy_live: HashMap<&str, u64> =
        buddy_files.iter().filter(|f| !f.deleted).map(|f| (f.path.as_str(), f.size)).collect();

    let checked = manifest.entries.len();
    let known_paths: HashSet<String> = manifest.entries.keys().cloned().collect();

    let mut missing_on_buddy = Vec::new();
    for (path, entry) in &manifest.entries {
        let landed_correctly =
            buddy_live.get(path.as_str()).is_some_and(|&size| size == entry.ciphertext_size);
        if !landed_correctly {
            missing_on_buddy.push(path.clone());
        }
    }
    for path in &missing_on_buddy {
        manifest.entries.remove(path);
    }
    if !missing_on_buddy.is_empty() {
        manifest
            .save(data_dir, &buddy_node_id)
            .await
            .context("failed to persist manifest after reconciliation")?;
    }

    let unexpected_on_buddy: Vec<String> = buddy_live
        .keys()
        .filter(|path| !known_paths.contains(**path))
        .map(|path| path.to_string())
        .collect();

    Ok(ReconcileStats { checked, missing_on_buddy, unexpected_on_buddy })
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

/// File count and total (plaintext) bytes under `backup_dir` right now —
/// for the dashboard's "This Device" panel, not the sync protocol. A
/// plain re-scan rather than anything manifest-based, since this is meant
/// to show what's actually sitting in the directory, independent of
/// whether it's been backed up to any particular buddy yet.
pub async fn local_backup_summary(backup_dir: &Path) -> Result<(usize, u64)> {
    let files = scan_local_files(backup_dir).await?;
    let total_bytes = files.iter().map(|(_, _, size)| size).sum();
    Ok((files.len(), total_bytes))
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
