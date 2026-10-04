use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl};
use sha2::{Digest, Sha256};

use crate::index::{self, SentEntry};
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
    backup_dir: &Path,
    buddy_id: EndpointId,
) -> Result<BackupCycleStats> {
    let buddy_node_id = buddy_id.to_string();
    let idx = index::get();
    let sent = idx.sent_entries(&buddy_node_id)?;

    let scan = scan_local_files(backup_dir).await?;
    let local_files = scan.files;

    // A folder that suddenly looks completely empty while the buddy holds
    // files from it is far more likely to be a drive that isn't mounted (or
    // a broken mount) than someone deliberately deleting everything —
    // mirroring it would send a delete for every file. Stop and say so
    // instead; ALLOW_EMPTY_BACKUP_DIR=true overrides it for the real thing.
    if looks_like_missing_drive(local_files.len(), scan.unreadable.len(), sent.len())
        && !allow_empty_backup_dir()
    {
        anyhow::bail!(
            "your backup folder ({}) looks empty, but {} file(s) from it are backed up with this buddy — \
             not deleting them all. Is the drive with your files mounted? If you really did delete \
             everything, set ALLOW_EMPTY_BACKUP_DIR=true in .env and restart.",
            backup_dir.display(),
            sent.len()
        );
    }

    // Problems on this side (files or folders we can't read) are reported
    // like any other failed file, but don't stop the rest of the cycle.
    let mut files_failed = 0usize;
    let mut failed_paths = Vec::new();
    for path in &scan.unreadable {
        files_failed += 1;
        if failed_paths.len() < MAX_FAILED_PATHS_SHOWN {
            failed_paths.push(FailedFile {
                path: if path.is_empty() { "(backup folder)".to_string() } else { path.clone() },
                reason: Some("can't open this on this device — check permissions; deletions paused until fixed"),
            });
        }
    }

    // Change detection: a file is only read (hashed) when its size or
    // modified time differs from what the index recorded last time. For an
    // unchanged folder a cycle is a metadata-only listing, however big the
    // files are.
    let mut to_put: Vec<(String, PathBuf, u64, String)> = Vec::new();
    for f in &local_files {
        let sha256 = match idx.cached_hash(&f.rel_path, f.size, f.mtime_ns)? {
            Some(hash) => hash,
            None => match crate::crypto::sha256_file(f.abs_path.clone()).await {
                Ok(hash) => {
                    idx.remember_hash(&f.rel_path, f.size, f.mtime_ns, &hash)?;
                    hash
                }
                Err(err) => {
                    // One unreadable file used to abort the whole cycle (so
                    // nothing else got backed up either). It stays in
                    // local_paths below, so it's never mistaken for deleted.
                    tracing::warn!(path = %f.rel_path, ?err, "can't read file — skipping it this cycle");
                    files_failed += 1;
                    if failed_paths.len() < MAX_FAILED_PATHS_SHOWN {
                        failed_paths.push(FailedFile {
                            path: f.rel_path.clone(),
                            reason: Some("can't read this file on this device — check its permissions"),
                        });
                    }
                    continue;
                }
            },
        };
        let changed = sent.get(&f.rel_path).is_none_or(|e| e.sha256 != sha256 || e.size != f.size);
        if changed {
            to_put.push((f.rel_path.clone(), f.abs_path.clone(), f.size, sha256));
        }
    }

    let local_paths: HashSet<&String> = local_files.iter().map(|f| &f.rel_path).collect();
    if scan.unreadable.is_empty() {
        idx.prune_hashes(&local_paths)?;
    } else {
        tracing::warn!(
            unreadable = scan.unreadable.len(),
            "parts of the backup folder couldn't be read — not mirroring any deletions this cycle"
        );
    }
    let to_delete = deletions_to_mirror(sent.keys(), &local_paths, scan.unreadable.len());

    // Nothing to send and nothing wrong locally: skip the connection (the
    // caller pings instead). With local failures, still connect, so the
    // dashboard's reachability/staleness stays based on a real connection.
    if to_put.is_empty() && to_delete.is_empty() && files_failed == 0 {
        return Ok(BackupCycleStats::default());
    }

    let cycle_started = std::time::Instant::now();
    let addr = EndpointAddr::new(buddy_id).with_relay_url(relay_url.clone());
    let conn = endpoint.connect(addr, protocol::ALPN).await.map_err(protocol::explain_connect_error)?;

    // Streaming uploads need the buddy on client 0.5.0+; older buddies get
    // the old whole-file upload (size-limited, see LEGACY_PUT_MAX_BYTES).
    let streaming = to_put.is_empty() || buddy_protocol_version(&conn).await >= 2;
    if !streaming {
        tracing::info!("buddy runs an older client — using the old upload format until they update");
    }

    let mut put_count = 0;
    for (rel_path, abs_path, size, sha256) in &to_put {
        if !streaming && *size > protocol::LEGACY_PUT_MAX_BYTES {
            files_failed += 1;
            if failed_paths.len() < MAX_FAILED_PATHS_SHOWN {
                failed_paths.push(FailedFile {
                    path: rel_path.clone(),
                    reason: Some("your buddy's client is out of date — files over 512 MB will send once they update"),
                });
            }
            continue;
        }
        let result = if streaming {
            put_one_file(&conn, rel_path, abs_path, *size).await
        } else {
            put_one_file_legacy(&conn, rel_path, abs_path, *size).await
        };
        match result {
            Ok(ciphertext_size) => {
                put_count += 1;
                // Recorded per file as it lands (one row, not a rewrite of
                // everything), so a cycle interrupted partway only re-sends
                // the file that was in flight.
                if let Err(err) = idx.record_sent(
                    &buddy_node_id,
                    rel_path,
                    &SentEntry { size: *size, sha256: sha256.clone(), ciphertext_size },
                ) {
                    tracing::warn!(path = %rel_path, ?err, "backed up file but failed to record it — may be re-sent next cycle");
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
                delete_count += 1;
                if let Err(err) = idx.forget_sent(&buddy_node_id, std::slice::from_ref(rel_path)) {
                    tracing::warn!(path = %rel_path, ?err, "told buddy about a deleted file but failed to record it — may retry next cycle");
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
    buddy_id: EndpointId,
) -> Result<ReconcileStats> {
    let buddy_node_id = buddy_id.to_string();
    let idx = index::get();
    let sent = idx.sent_entries(&buddy_node_id)?;
    if sent.is_empty() {
        // Nothing we believe we've sent this buddy, so nothing to check
        // — and no reason to open a connection just to confirm that.
        return Ok(ReconcileStats::default());
    }

    let buddy_files = crate::restore::list_buddy_files(endpoint, relay_url, buddy_id)
        .await
        .context("failed to list buddy's files for reconciliation")?;

    // Only live files count as "the buddy still has this" — a
    // `deleted: true` entry is a `.versions/` backup slot from something
    // that was overwritten or deleted, not the live file we're tracking
    // (see ListEntry's doc comment in protocol.rs).
    let buddy_live: HashMap<&str, u64> =
        buddy_files.iter().filter(|f| !f.deleted).map(|f| (f.path.as_str(), f.size)).collect();

    let checked = sent.len();
    let missing_on_buddy: Vec<String> = sent
        .iter()
        .filter(|(path, entry)| {
            !buddy_live.get(path.as_str()).is_some_and(|&size| size == entry.ciphertext_size)
        })
        .map(|(path, _)| path.clone())
        .collect();
    // Dropping them from the index makes the very next ordinary cycle see
    // those local files as changed and re-send them.
    if !missing_on_buddy.is_empty() {
        idx.forget_sent(&buddy_node_id, &missing_on_buddy)
            .context("failed to record reconciliation result")?;
    }

    let unexpected_on_buddy: Vec<String> = buddy_live
        .keys()
        .filter(|path| !sent.contains_key(**path))
        .map(|path| path.to_string())
        .collect();

    Ok(ReconcileStats { checked, missing_on_buddy, unexpected_on_buddy })
}

/// Encrypts and uploads one file as a stream (protocol v2 PutStream), in
/// bounded memory whatever its size. Returns the encrypted size stored on
/// the buddy's side, which the index records for the dashboard and for
/// reconciliation.
async fn put_one_file(
    conn: &iroh::endpoint::Connection,
    rel_path: &str,
    abs_path: &Path,
    plaintext_len: u64,
) -> Result<u64> {
    let mut enc = crate::crypto::encrypt_file(abs_path.to_path_buf(), plaintext_len);
    let ciphertext_len = match (&mut enc.ciphertext_len).await {
        Ok(len) => len,
        // Encryption ended before it could start (e.g. the file can't be
        // opened) — report why.
        Err(_) => {
            return Err(enc.done.await.context("encryption task failed")?.err().unwrap_or_else(|| {
                anyhow::anyhow!("encryption stopped unexpectedly")
            }));
        }
    };

    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(
        &mut send,
        &SyncRequest::PutStream { path: rel_path.to_string(), ciphertext_len },
    )
    .await?;

    let mut hasher = Sha256::new();
    let mut sent = 0u64;
    while let Some(chunk) = enc.chunks.recv().await {
        hasher.update(&chunk);
        sent += chunk.len() as u64;
        // A rejection by the buddy (pledge full, disk full…) arrives as
        // STOP_SENDING and fails this write — see reject_reason_from_err.
        send.write_all(&chunk).await.context("failed to send file bytes")?;
    }
    enc.done.await.context("encryption task failed")??;
    if sent != ciphertext_len {
        anyhow::bail!("sent {sent} bytes but announced {ciphertext_len} — aborting this file");
    }
    protocol::write_frame(
        &mut send,
        &protocol::PutTrailer { ciphertext_sha256: hex::encode(hasher.finalize()) },
    )
    .await?;
    send.finish().context("failed to finish send stream")?;

    let ack: SyncAck = protocol::read_frame(&mut recv).await.context("failed to read ack")?;
    if !ack.ok {
        anyhow::bail!("buddy rejected file: {}", ack.error.unwrap_or_default());
    }
    Ok(ciphertext_len)
}

/// Asks the buddy which protocol version it speaks. A client older than
/// 0.5.0 can't parse the question and drops the stream, which reads as an
/// error here — that *is* the answer (version 1).
async fn buddy_protocol_version(conn: &iroh::endpoint::Connection) -> u32 {
    let ask = async {
        let (mut send, mut recv) = conn.open_bi().await?;
        protocol::write_frame(&mut send, &SyncRequest::Hello).await?;
        send.finish()?;
        let resp: protocol::HelloResponse = protocol::read_frame(&mut recv).await?;
        anyhow::Ok(resp.protocol_version)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(15), ask).await {
        Ok(Ok(v)) => v,
        _ => 1,
    }
}

/// The pre-0.5.0 upload, for buddies that haven't updated: the hash goes
/// in the header, so the encrypted file is assembled in memory first.
/// Callers keep this to files under LEGACY_PUT_MAX_BYTES.
async fn put_one_file_legacy(
    conn: &iroh::endpoint::Connection,
    rel_path: &str,
    abs_path: &Path,
    plaintext_len: u64,
) -> Result<u64> {
    let mut enc = crate::crypto::encrypt_file(abs_path.to_path_buf(), plaintext_len);
    let mut ciphertext = Vec::new();
    while let Some(chunk) = enc.chunks.recv().await {
        ciphertext.extend_from_slice(&chunk);
    }
    enc.done.await.context("encryption task failed")??;
    let ciphertext_len = ciphertext.len() as u64;

    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(
        &mut send,
        &SyncRequest::Put {
            path: rel_path.to_string(),
            ciphertext_len,
            ciphertext_sha256: hex::encode(Sha256::digest(&ciphertext)),
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

/// File count and total (plaintext) bytes under `backup_dir` right now —
/// for the dashboard's "This Device" panel, not the sync protocol. A
/// plain re-scan rather than anything manifest-based, since this is meant
/// to show what's actually sitting in the directory, independent of
/// whether it's been backed up to any particular buddy yet.
pub async fn local_backup_summary(backup_dir: &Path) -> Result<(usize, u64)> {
    let files = scan_local_files(backup_dir).await?.files;
    let total_bytes = files.iter().map(|f| f.size).sum();
    Ok((files.len(), total_bytes))
}

/// One file found in BACKUP_DIR. `mtime_ns` is its modified time in
/// nanoseconds since the epoch (-1 if unavailable, which never matches a
/// cached entry, so the file is simply re-hashed).
struct LocalFile {
    rel_path: String,
    abs_path: PathBuf,
    size: u64,
    mtime_ns: i64,
}

/// What a scan of BACKUP_DIR found: the files it could see, plus anything
/// it couldn't open (relative paths). Unreadable entries matter: a folder
/// the client isn't allowed to open looks exactly like a folder that was
/// deleted, so callers must not treat "not in `files`" as "gone" while
/// `unreadable` is non-empty.
struct LocalScan {
    files: Vec<LocalFile>,
    unreadable: Vec<String>,
}

fn rel_to(backup_dir: &Path, abs_path: &Path) -> String {
    abs_path
        .strip_prefix(backup_dir)
        .unwrap_or(abs_path)
        .to_string_lossy()
        .replace('\\', "/") // normalize separators in case this ever runs on Windows
}

// Metadata only — no file contents are read here. Runs on a blocking
// thread, since walking a large folder can take a while.
async fn scan_local_files(backup_dir: &Path) -> Result<LocalScan> {
    let backup_dir = backup_dir.to_path_buf();
    tokio::task::spawn_blocking(move || scan_local_files_blocking(&backup_dir))
        .await
        .context("folder scan task failed")
}

fn scan_local_files_blocking(backup_dir: &Path) -> LocalScan {
    let mut out = Vec::new();
    let mut unreadable = Vec::new();
    for entry in walkdir::WalkDir::new(backup_dir).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                // Permission denied on a folder (or a file vanishing mid-walk).
                // Recorded rather than skipped silently — see LocalScan.
                let path = err.path().map(|p| rel_to(backup_dir, p)).unwrap_or_default();
                tracing::warn!(path = %path, %err, "can't read part of BACKUP_DIR");
                unreadable.push(path);
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let abs_path = entry.path().to_path_buf();
        let rel_path = rel_to(backup_dir, &abs_path);
        let meta = entry.metadata().ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime_ns = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(-1);
        out.push(LocalFile { rel_path, abs_path, size, mtime_ns });
    }
    LocalScan { files: out, unreadable }
}

/// True when BACKUP_DIR scanned completely (nothing unreadable) yet came
/// back empty while the buddy holds files from it — the signature of a
/// drive that isn't mounted rather than a real "delete everything".
fn looks_like_missing_drive(local_files: usize, unreadable: usize, backed_up: usize) -> bool {
    local_files == 0 && unreadable == 0 && backed_up > 0
}

/// Which backed-up paths to delete from the buddy. Anything under a folder
/// we couldn't open is missing from `local_paths` without having been
/// deleted, so there are no deletions at all while any part of the scan
/// failed — new and changed files still go out.
fn deletions_to_mirror<'a>(
    backed_up: impl Iterator<Item = &'a String>,
    local_paths: &HashSet<&String>,
    unreadable: usize,
) -> Vec<String> {
    if unreadable > 0 {
        return Vec::new();
    }
    backed_up.filter(|rel| !local_paths.contains(rel)).cloned().collect()
}

/// ALLOW_EMPTY_BACKUP_DIR=true lets a cycle mirror "everything was deleted"
/// to the buddy. Off by default — see the empty-folder guard in
/// run_backup_cycle.
fn allow_empty_backup_dir() -> bool {
    matches!(
        std::env::var("ALLOW_EMPTY_BACKUP_DIR").map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Ok("1" | "true" | "yes")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bb-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_folder_with_backed_up_files_looks_like_missing_drive() {
        assert!(looks_like_missing_drive(0, 0, 12));
        assert!(!looks_like_missing_drive(0, 0, 0), "a fresh, empty folder is fine");
        assert!(!looks_like_missing_drive(3, 0, 12), "some files present: ordinary deletions");
        assert!(!looks_like_missing_drive(0, 1, 12), "unreadable is handled by pausing deletions instead");
    }

    #[test]
    fn deletions_pause_while_anything_is_unreadable() {
        let backed_up = ["a.txt".to_string(), "photos/b.jpg".to_string()];
        let present = "a.txt".to_string();
        let local: HashSet<&String> = [&present].into_iter().collect();
        assert_eq!(deletions_to_mirror(backed_up.iter(), &local, 0), vec!["photos/b.jpg".to_string()]);
        assert!(deletions_to_mirror(backed_up.iter(), &local, 1).is_empty());
    }

    // Root ignores permissions, so this only proves anything when run as a
    // normal user; as root it returns early rather than giving a false pass.
    #[tokio::test]
    async fn unreadable_folder_is_reported_not_treated_as_deleted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp("scan");
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("b.txt"), b"b").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable_anyway = std::fs::read_dir(&locked).is_ok();

        let scan = scan_local_files(&dir).await.unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        if readable_anyway {
            eprintln!("running as root: permission check skipped");
            return;
        }
        let names: Vec<&str> = scan.files.iter().map(|f| f.rel_path.as_str()).collect();
        assert_eq!(names, vec!["a.txt"]);
        assert_eq!(scan.unreadable, vec!["locked".to_string()]);
    }
}
