use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::bandwidth::BandwidthBook;
use crate::netinfo::{self, ConnectionQuality};
use crate::protocol::{
    self, DiskStatsResponse, ListEntry, ListResponse, ListVersionsResponse, PutRejectReason,
    SyncAck, SyncRequest, VersionEntry,
};

// Never let incoming buddy data consume the disk down to nothing, no
// matter what's been pledged — pledges are numbers two people agreed on
// over the API, not a reservation the OS enforces, and an over-pledge (or
// just something else on the same disk growing) shouldn't be able to take
// this host down. The floor is whichever is bigger: a flat 2GB, or 5% of
// the filesystem's total size, so it scales sensibly from a small test
// box up to a many-terabyte server.
const MIN_FREE_RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MIN_FREE_RESERVE_PERCENT: u64 = 5;

// Real, physical headroom check — independent of and in addition to the
// per-buddy pledge enforcement in handle_put below. Fails open (allows
// the write) if disk stats can't be read at all, since a transient statvfs
// error shouldn't block every incoming backup; the pledge check is still
// the primary guard in that case.
//
// Measured on the filesystem holding BUDDY_FILES_DIR — where incoming data
// actually lands — not DATA_DIR, since the two can be different disks.
async fn disk_has_room_for(buddy_files_dir: &Path, additional_bytes: u64) -> bool {
    let buddy_files_dir = buddy_files_dir.to_path_buf();
    match tokio::task::spawn_blocking(move || fs4::statvfs(&buddy_files_dir)).await {
        Ok(Ok(stats)) => {
            let reserve = MIN_FREE_RESERVE_BYTES.max(stats.total_space() / 100 * MIN_FREE_RESERVE_PERCENT);
            let usable = stats.available_space().saturating_sub(reserve);
            additional_bytes <= usable
        }
        _ => true,
    }
}

// How many *previous* copies of a file are kept once it's overwritten or
// deleted, on top of the live one — 2 backups + the live copy = 3 versions
// total recoverable at any time. A pledge or the real-disk floor above
// still bounds how much space any of this can consume; this just bounds
// the multiplier a single churning file can add on top of its live size,
// so "someone keeps editing one huge file" can't quietly balloon disk use
// without limit.
const MAX_BACKUP_VERSIONS: u32 = 2;

fn versions_root(sender_dir: &Path) -> PathBuf {
    sender_dir.join(".versions")
}

fn version_path(sender_dir: &Path, rel_path: &str, slot: u32) -> Option<PathBuf> {
    safe_join(&versions_root(sender_dir), &format!("{rel_path}.v{slot}"))
}

// Called just before a path's live content is about to be replaced
// (overwrite) or removed (delete). Shifts existing backup slots down by
// one (dropping the oldest once MAX_BACKUP_VERSIONS is reached), then
// moves the *current* live file into the newest slot — so a delete isn't
// destructive (the deleted content becomes a recoverable backup) and an
// overwrite doesn't lose what was there before. A no-op if there's no
// live file yet (first write for this path — nothing to protect).
async fn rotate_versions(sender_dir: &Path, rel_path: &str) {
    let Some(live) = safe_join(sender_dir, rel_path) else { return };
    if tokio::fs::metadata(&live).await.is_err() {
        return;
    }

    if let Some(oldest) = version_path(sender_dir, rel_path, MAX_BACKUP_VERSIONS) {
        let _ = tokio::fs::remove_file(&oldest).await;
    }
    for slot in (1..MAX_BACKUP_VERSIONS).rev() {
        let (Some(from), Some(to)) =
            (version_path(sender_dir, rel_path, slot), version_path(sender_dir, rel_path, slot + 1))
        else {
            continue;
        };
        if tokio::fs::metadata(&from).await.is_ok() {
            if let Some(parent) = to.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            let _ = tokio::fs::rename(&from, &to).await;
        }
    }
    if let Some(slot1) = version_path(sender_dir, rel_path, 1) {
        if let Some(parent) = slot1.parent()
            && let Err(err) = tokio::fs::create_dir_all(parent).await
        {
            tracing::warn!(path = %rel_path, ?err, "couldn't create version directory — not keeping a backup of this overwrite");
            return;
        }
        if let Err(err) = tokio::fs::rename(&live, &slot1).await {
            tracing::warn!(path = %rel_path, ?err, "couldn't rotate previous version into a backup slot");
        }
    }
}

// How much of a buddy's pledge we've used (received_bytes) against how
// much they've actually pledged us (pledged_bytes). Keyed by the buddy's
// Iroh node id (EndpointId's string form).
#[derive(Clone, Copy, Default)]
pub struct PledgeState {
    pub pledged_bytes: u64,
    pub received_bytes: u64,
}

// Shared between the accept loop (enforces the pledge on incoming data)
// and the outbound poll loop in main.rs (refreshes pledged_bytes from the
// API every 30s). A plain std::sync::Mutex is fine: every critical section
// is a quick map read/write with no .await inside it.
pub type PledgeBook = Arc<Mutex<HashMap<String, PledgeState>>>;

// received_bytes must survive restarts — otherwise a buddy could get a
// fresh budget just by waiting for us to redeploy. Persisted as plain JSON
// at DATA_DIR/usage.json: { "<node_id>": <received_bytes> }. pledged_bytes
// itself isn't persisted — it's re-learned from the API on every poll.
pub async fn load_usage_ledger(data_dir: &Path) -> HashMap<String, u64> {
    match tokio::fs::read(data_dir.join("usage.json")).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

pub async fn save_usage_ledger(data_dir: &Path, book: &PledgeBook) -> Result<()> {
    let snapshot: HashMap<String, u64> = {
        let book = book.lock().unwrap();
        book.iter().map(|(id, state)| (id.clone(), state.received_bytes)).collect()
    };
    let json = serde_json::to_vec(&snapshot)?;
    tokio::fs::write(data_dir.join("usage.json"), json).await?;
    Ok(())
}

/// Accept-loop entry point for one incoming connection. A buddy's client
/// opens one bidirectional stream per request (Ping, Put, Delete, List, or
/// Get — see protocol.rs), potentially many of them over the lifetime of
/// one connection (one Put per changed file in a backup cycle), so this
/// keeps accepting streams until the peer closes the connection rather
/// than handling just one and returning.
///
/// Storage is scoped by sender: everything a given buddy sends us lives
/// under BUDDY_FILES_DIR/<their node id>/<path they gave us>, and List/Get only
/// ever look inside that one sender's own directory — a buddy can never
/// see or fetch what another buddy stored with us.
pub async fn handle_incoming(
    incoming: iroh::endpoint::Incoming,
    data_dir: PathBuf,
    buddy_files_dir: PathBuf,
    pledge_book: PledgeBook,
    bandwidth_book: BandwidthBook,
) -> Result<()> {
    let connecting = incoming.accept()?;
    let conn = connecting.await?;
    let remote = conn.remote_id();
    let remote_id = remote.to_string();
    let sender_dir = buddy_files_dir.join(&remote_id);
    // Read once per connection rather than per stream — the path a
    // connection settles on doesn't change mid-connection, and every
    // stream on it (one per file in a backup cycle) shares the same one.
    let quality = netinfo::connection_quality(&conn);

    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => break, // peer closed the connection — normal end of a sync cycle
        };
        let data_dir = data_dir.clone();
        let buddy_files_dir = buddy_files_dir.clone();
        let sender_dir = sender_dir.clone();
        let pledge_book = pledge_book.clone();
        let bandwidth_book = bandwidth_book.clone();
        let remote_id = remote_id.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_stream(
                send,
                recv,
                data_dir,
                buddy_files_dir,
                sender_dir,
                remote_id.clone(),
                pledge_book,
                bandwidth_book,
                quality,
            )
            .await
            {
                tracing::warn!(from = %remote_id, ?err, "request from buddy failed");
            }
        });
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_stream(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    data_dir: PathBuf,
    buddy_files_dir: PathBuf,
    sender_dir: PathBuf,
    remote_id: String,
    pledge_book: PledgeBook,
    bandwidth_book: BandwidthBook,
    quality: ConnectionQuality,
) -> Result<()> {
    let request: SyncRequest =
        protocol::read_frame(&mut recv).await.context("failed to read request")?;

    match request {
        SyncRequest::Ping => {
            protocol::write_frame(&mut send, &SyncAck::ok()).await?;
        }
        SyncRequest::Hello => {
            protocol::write_frame(
                &mut send,
                &protocol::HelloResponse { protocol_version: protocol::PROTOCOL_VERSION },
            )
            .await?;
        }
        // v1 senders state the hash up front; v2 senders (PutStream) send
        // it after the bytes. Both are handled by the same streaming code.
        SyncRequest::Put { path, ciphertext_len, ciphertext_sha256 } => {
            handle_put(
                &mut send,
                &mut recv,
                &sender_dir,
                &remote_id,
                &pledge_book,
                &data_dir,
                &buddy_files_dir,
                &bandwidth_book,
                &quality,
                path,
                ciphertext_len,
                Some(ciphertext_sha256),
            )
            .await?;
        }
        SyncRequest::PutStream { path, ciphertext_len } => {
            handle_put(
                &mut send,
                &mut recv,
                &sender_dir,
                &remote_id,
                &pledge_book,
                &data_dir,
                &buddy_files_dir,
                &bandwidth_book,
                &quality,
                path,
                ciphertext_len,
                None,
            )
            .await?;
        }
        SyncRequest::Delete { path } => {
            handle_delete(&mut send, &sender_dir, &remote_id, &pledge_book, &data_dir, path).await?;
        }
        SyncRequest::List => {
            handle_list(&mut send, &sender_dir).await?;
        }
        SyncRequest::ListStream => {
            let files = list_sender_files(&sender_dir).await;
            let mut chunks = files.chunks(protocol::LIST_CHUNK_ENTRIES).peekable();
            if chunks.peek().is_none() {
                protocol::write_frame(&mut send, &protocol::ListChunk { files: Vec::new(), done: true }).await?;
            }
            while let Some(chunk) = chunks.next() {
                let done = chunks.peek().is_none();
                // ListEntry isn't Clone; rebuild the slice's entries.
                let files = chunk
                    .iter()
                    .map(|e| ListEntry { path: e.path.clone(), size: e.size, deleted: e.deleted })
                    .collect();
                protocol::write_frame(&mut send, &protocol::ListChunk { files, done }).await?;
            }
        }
        SyncRequest::Get { path } => {
            handle_get(&mut send, &sender_dir, path, &data_dir, &bandwidth_book, &quality).await?;
        }
        SyncRequest::DiskStats => {
            handle_disk_stats(&mut send, &buddy_files_dir, &pledge_book).await?;
        }
        SyncRequest::ListVersions { path } => {
            handle_list_versions(&mut send, &sender_dir, path).await?;
        }
        SyncRequest::GetVersion { path, version } => {
            handle_get_version(&mut send, &sender_dir, path, version, &data_dir, &bandwidth_book, &quality)
                .await?;
        }
    }

    send.stopped().await.ok();
    Ok(())
}

// Reject anything that would escape the sender's own directory — an
// absolute path or any ".." component. A malicious or buggy buddy client
// sending path: "../../etc/passwd" should get "invalid path", not a write
// outside BUDDY_FILES_DIR/<their id>/. Also reused by restore.rs, which joins
// paths from a buddy's own List response the same way.
pub(crate) fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return None;
    }
    if rel_path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return None;
    }
    Some(base.join(rel_path))
}

#[allow(clippy::too_many_arguments)]
async fn handle_put(
    send: &mut iroh::endpoint::SendStream,
    recv: &mut iroh::endpoint::RecvStream,
    sender_dir: &Path,
    remote_id: &str,
    pledge_book: &PledgeBook,
    data_dir: &Path,
    buddy_files_dir: &Path,
    bandwidth_book: &BandwidthBook,
    quality: &ConnectionQuality,
    path: String,
    ciphertext_len: u64,
    expected_sha256: Option<String>,
) -> Result<()> {
    if ciphertext_len > protocol::MAX_FILE_BYTES {
        reject_without_reading_body(recv, send, PutRejectReason::TooLarge).await?;
        return Ok(());
    }

    let Some(dest) = safe_join(sender_dir, &path) else {
        reject_without_reading_body(recv, send, PutRejectReason::InvalidPath).await?;
        return Ok(());
    };

    // If we already have a file at this path, only the *delta* against its
    // current size should count against the pledge — otherwise every
    // re-send of an unchanged-size file would eat into the budget again.
    // Note this slightly undercounts real disk growth while a path's
    // backup slots are still filling up (an overwrite moves the old
    // content into .versions/ rather than freeing it — see
    // rotate_versions), since the pledge math below still assumes the
    // previous size is reclaimed. Once a path has churned through all
    // MAX_BACKUP_VERSIONS slots that's true again (the oldest slot really
    // is freed on every rotation after that), and either way the real-disk
    // floor just below is what actually prevents the disk from filling —
    // this delta is only ever used to decide whether a pledge is exceeded.
    let previous_size = tokio::fs::metadata(&dest).await.map(|m| m.len()).unwrap_or(0);
    let incoming_delta = ciphertext_len.saturating_sub(previous_size);

    // The real disk guard, ahead of the pledge check below: a pledge is
    // just a number this host agreed to over the API, so a buddy honoring
    // their own pledge perfectly can still arrive at a disk that's full
    // for some other reason (an over-generous pledge, something else on
    // the same volume). Checked first so a failure here reads as "no
    // room" rather than the more specific "pledge exceeded".
    if incoming_delta > 0 && !disk_has_room_for(buddy_files_dir, incoming_delta).await {
        tracing::warn!(
            from = %remote_id,
            path = %path,
            attempted_bytes = ciphertext_len,
            "rejected file — not enough real disk space left on this host"
        );
        reject_without_reading_body(recv, send, PutRejectReason::DiskFull).await?;
        return Ok(());
    }

    let decision = {
        let mut book = pledge_book.lock().unwrap();
        let state = book.entry(remote_id.to_string()).or_default();
        let projected =
            state.received_bytes.saturating_sub(previous_size).saturating_add(ciphertext_len);
        if projected > state.pledged_bytes {
            Err((state.pledged_bytes, state.received_bytes))
        } else {
            state.received_bytes = projected;
            Ok(())
        }
    };

    let Err((pledged_bytes, received_bytes)) = decision else {
        let rollback = || {
            let mut book = pledge_book.lock().unwrap();
            if let Some(state) = book.get_mut(remote_id) {
                state.received_bytes =
                    state.received_bytes.saturating_sub(ciphertext_len).saturating_add(previous_size);
            }
        };

        // Stream the body into a temp file in this buddy's own folder (same
        // filesystem, so the final move is an atomic rename), hashing as it
        // arrives. Memory use is one buffer, whatever the file size — this
        // used to read the whole file into RAM.
        let tmp = match incoming_tmp_path(sender_dir).await {
            Ok(tmp) => tmp,
            Err(err) => {
                rollback();
                return Err(err);
            }
        };
        let received = receive_body(recv, &tmp, ciphertext_len).await;
        let actual_hash = match received {
            Ok(hash) => hash,
            Err(err) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                rollback();
                return Err(err).context("failed to read file bytes");
            }
        };

        // v2 (PutStream): the hash follows the bytes.
        let expected = match expected_sha256 {
            Some(hash) => hash,
            None => match protocol::read_frame::<protocol::PutTrailer>(recv).await {
                Ok(t) => t.ciphertext_sha256,
                Err(err) => {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    rollback();
                    return Err(err).context("failed to read upload trailer");
                }
            },
        };
        if actual_hash != expected {
            let _ = tokio::fs::remove_file(&tmp).await;
            rollback();
            protocol::write_frame(send, &SyncAck::err("hash mismatch")).await?;
            return Ok(());
        }

        // Protect whatever was there before this overwrite — a buddy's
        // backup cycle only Puts a path when their local copy actually
        // changed, so any existing `dest` here is genuinely being replaced,
        // not just re-sent unchanged.
        rotate_versions(sender_dir, &path).await;

        if let Some(parent) = dest.parent()
            && let Err(err) = tokio::fs::create_dir_all(parent).await
        {
            let _ = tokio::fs::remove_file(&tmp).await;
            rollback();
            return Err(err).context("failed to create directory for incoming file");
        }
        if let Err(err) = tokio::fs::rename(&tmp, &dest).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            rollback();
            return Err(err).context("failed to store incoming file");
        }

        if let Err(err) = save_usage_ledger(data_dir, pledge_book).await {
            // Don't fail the exchange over a ledger write hiccup — worst
            // case we under-persist by one file's worth and a restart
            // re-allows it, which is the safe direction to err in.
            tracing::warn!(?err, "failed to persist usage ledger");
        }
        crate::bandwidth::record(data_dir, bandwidth_book, quality, ciphertext_len).await;

        protocol::write_frame(send, &SyncAck::ok()).await?;
        tracing::info!(from = %remote_id, path = %path, bytes = ciphertext_len, "stored file from buddy");
        return Ok(());
    };

    tracing::warn!(
        from = %remote_id,
        path = %path,
        attempted_bytes = ciphertext_len,
        pledged_bytes,
        received_bytes,
        "rejected file — would exceed this buddy's pledge"
    );
    reject_without_reading_body(recv, send, PutRejectReason::PledgeExceeded).await?;
    Ok(())
}

// Every early-rejection branch above decides *before* reading the
// incoming file body off `recv` — deliberately, so we don't waste time
// and bandwidth downloading megabytes/gigabytes of a file we're about to
// throw away. But the sender (put_one_file in backup.rs) writes its
// entire ciphertext body before it ever tries to read our ack, so if we
// just write the rejection ack without doing anything about the body
// they're still sending, their `send.write_all(&ciphertext)` blocks
// forever waiting for QUIC flow-control credit we'll never grant (we're
// not reading), while we sit in handle_stream's `send.stopped().await`
// waiting for them to finish sending — a real deadlock, not just a slow
// cycle. `RecvStream::stop()` sends a QUIC STOP_SENDING frame, which
// makes their blocked write fail immediately instead of hanging, so they
// can log the failure and move on to the next file next cycle. The error
// code it carries is `reason`'s own code (see PutRejectReason), so the
// sender — which generally never gets to read the string ack below, since
// it's usually still mid-write when the stop lands — can still learn
// *why*, not just that it failed.
async fn reject_without_reading_body(
    recv: &mut iroh::endpoint::RecvStream,
    send: &mut iroh::endpoint::SendStream,
    reason: PutRejectReason,
) -> Result<()> {
    recv.stop(iroh::endpoint::VarInt::from_u32(reason.code())).ok();
    protocol::write_frame(send, &SyncAck::err(reason.message())).await
}

// Uploads in progress live here, inside the sender's own folder: same
// filesystem as the final location (so moving into place is an atomic
// rename), and never listed as files (see collect_files). Leftovers from an
// interrupted upload are cleared at startup (clear_incoming).
const INCOMING_DIR: &str = ".incoming";
const IO_CHUNK: usize = 256 * 1024;

async fn incoming_tmp_path(sender_dir: &Path) -> Result<PathBuf> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = sender_dir.join(INCOMING_DIR);
    tokio::fs::create_dir_all(&dir).await.context("failed to create upload staging folder")?;
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(dir.join(format!("{}-{n}.part", std::process::id())))
}

/// Reads exactly `len` bytes from `recv` into a new file at `path`,
/// returning their SHA-256. Flushed to disk before returning, so a file
/// isn't acknowledged until it's actually stored.
async fn receive_body(recv: &mut iroh::endpoint::RecvStream, path: &Path, len: u64) -> Result<String> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(path).await.context("failed to create upload file")?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; IO_CHUNK];
    let mut remaining = len;
    while remaining > 0 {
        let n = remaining.min(IO_CHUNK as u64) as usize;
        recv.read_exact(&mut buf[..n]).await.context("connection ended mid-file")?;
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await.context("failed to write incoming file")?;
        remaining -= n as u64;
    }
    file.sync_all().await.context("failed to flush incoming file")?;
    Ok(hex::encode(hasher.finalize()))
}

/// Removes leftovers of uploads interrupted by a restart or crash.
pub async fn clear_incoming(buddy_files_dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(buddy_files_dir).await else { return };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let staging = entry.path().join(INCOMING_DIR);
        if tokio::fs::metadata(&staging).await.is_ok() {
            let _ = tokio::fs::remove_dir_all(&staging).await;
        }
    }
}

/// Streams a stored file to `send` after an ok ack, in bounded memory.
/// Returns the number of bytes sent.
async fn send_file(send: &mut iroh::endpoint::SendStream, src: &Path) -> Result<Option<u64>> {
    use tokio::io::AsyncReadExt;
    let Ok(mut file) = tokio::fs::File::open(src).await else { return Ok(None) };
    protocol::write_frame(send, &SyncAck::ok()).await?;
    let mut buf = vec![0u8; IO_CHUNK];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf).await.context("failed to read stored file")?;
        if n == 0 {
            break;
        }
        send.write_all(&buf[..n]).await.context("failed to send file bytes")?;
        total += n as u64;
    }
    // Without finish(), the stream's FIN never goes out and the requester
    // (restore.rs) waits forever for the end of the file.
    send.finish().context("failed to finish send stream")?;
    Ok(Some(total))
}

async fn handle_delete(
    send: &mut iroh::endpoint::SendStream,
    sender_dir: &Path,
    remote_id: &str,
    pledge_book: &PledgeBook,
    data_dir: &Path,
    path: String,
) -> Result<()> {
    let Some(target) = safe_join(sender_dir, &path) else {
        protocol::write_frame(send, &SyncAck::err("invalid path")).await?;
        return Ok(());
    };

    let freed = tokio::fs::metadata(&target).await.map(|m| m.len()).unwrap_or(0);

    // A "delete" doesn't destroy the content — it moves it into the same
    // backup-slot rotation an overwrite goes through, so an accidental
    // local delete (the far more common case than a deliberate one) is
    // still recoverable from a backup slot, not actually gone. If that
    // rotation couldn't happen for some reason, fall back to a plain
    // remove so the live path is still reliably cleared either way.
    rotate_versions(sender_dir, &path).await;
    match tokio::fs::remove_file(&target).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {} // idempotent: already gone (or just rotated away) is fine
        Err(err) => {
            protocol::write_frame(send, &SyncAck::err(format!("failed to delete: {err}"))).await?;
            return Ok(());
        }
    }

    if freed > 0 {
        {
            let mut book = pledge_book.lock().unwrap();
            if let Some(state) = book.get_mut(remote_id) {
                state.received_bytes = state.received_bytes.saturating_sub(freed);
            }
        }
        if let Err(err) = save_usage_ledger(data_dir, pledge_book).await {
            tracing::warn!(?err, "failed to persist usage ledger after delete");
        }
    }

    protocol::write_frame(send, &SyncAck::ok()).await?;
    tracing::info!(from = %remote_id, path = %path, "deleted file for buddy");
    Ok(())
}

async fn handle_list(send: &mut iroh::endpoint::SendStream, sender_dir: &Path) -> Result<()> {
    let files = list_sender_files(sender_dir).await;
    protocol::write_frame(send, &ListResponse { files }).await?;
    Ok(())
}

// Answers DiskStats with this host's real numbers — see
// DiskStatsResponse's doc comment for why this exists (lets the asking
// buddy's dashboard show "what do they actually have" next to "what did
// they pledge" instead of just trusting the pledge).
async fn handle_disk_stats(
    send: &mut iroh::endpoint::SendStream,
    buddy_files_dir: &Path,
    pledge_book: &PledgeBook,
) -> Result<()> {
    let pledged_out_total_bytes: u64 =
        { pledge_book.lock().unwrap().values().map(|s| s.pledged_bytes).sum() };

    // The disk a buddy's files actually land on (BUDDY_FILES_DIR), which may
    // be a different drive from DATA_DIR's config/state.
    let dir_owned = buddy_files_dir.to_path_buf();
    let stats = tokio::task::spawn_blocking(move || fs4::statvfs(&dir_owned)).await;

    let response = match stats {
        Ok(Ok(stats)) => {
            let total_bytes = stats.total_space();
            let available_bytes = stats.available_space();
            DiskStatsResponse {
                total_bytes,
                available_bytes,
                used_bytes: total_bytes.saturating_sub(available_bytes),
                pledged_out_total_bytes,
            }
        }
        // Couldn't read disk stats — report zeros rather than failing the
        // whole request, so the asking dashboard can show "unknown"
        // instead of an error.
        _ => DiskStatsResponse {
            total_bytes: 0,
            available_bytes: 0,
            used_bytes: 0,
            pledged_out_total_bytes,
        },
    };

    protocol::write_frame(send, &response).await?;
    Ok(())
}

/// Everything stored under one sender's directory (received/<node id>/),
/// as the same ListEntry shape the List protocol request returns. Shared
/// with dashboard.rs so the local status page can show "what have they
/// backed up with us" without duplicating the directory walk.
///
/// Includes deleted-but-recoverable paths (live file gone, but a slot
/// still exists under .versions/) as entries with `deleted: true`, so a
/// file someone deleted doesn't just vanish from the list — it's still
/// reachable via History/restore-a-version, same as an overwritten file
/// is. Without this, rotate_versions() on delete faithfully keeps the
/// backup slot on disk, but nothing in the browser ever offered a way
/// back to it once the live copy was gone.
pub async fn list_sender_files(sender_dir: &Path) -> Vec<ListEntry> {
    let mut files = Vec::new();
    collect_files(sender_dir, sender_dir, &mut files).await;

    let live: std::collections::HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let mut deleted = Vec::new();
    collect_deleted(sender_dir, &versions_root(sender_dir), &live, &mut deleted).await;
    files.extend(deleted);

    files
}

// Walks .versions/ looking for paths that no longer have a live file —
// i.e. the file was deleted, not just overwritten. Each one becomes a
// single `deleted: true` ListEntry (deduped across its remaining backup
// slots), sized from whichever slot is newest.
fn collect_deleted<'a>(
    sender_dir: &'a Path,
    dir: &'a Path,
    live: &'a std::collections::HashSet<&'a str>,
    out: &'a mut Vec<ListEntry>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else { return };
        let mut seen = std::collections::HashSet::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let Ok(file_type) = entry.file_type().await else { continue };
            if file_type.is_dir() {
                collect_deleted(sender_dir, &path, live, out).await;
            } else if file_type.is_file() {
                let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else { continue };
                // Version slots are named "<original file name>.v<N>" —
                // strip that suffix to get back the original relative path.
                let Some(stripped) = file_name.rsplit_once(".v").and_then(|(name, slot)| {
                    slot.parse::<u32>().ok().map(|_| name)
                }) else {
                    continue;
                };
                let versions_dir = versions_root(sender_dir);
                let rel_dir = path.parent().unwrap_or(&path).strip_prefix(&versions_dir).unwrap_or(Path::new(""));
                let rel = rel_dir.join(stripped).to_string_lossy().replace('\\', "/");

                if live.contains(rel.as_str()) || !seen.insert(rel.clone()) {
                    continue;
                }
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                out.push(ListEntry { path: rel, size, deleted: true });
            }
        }
    })
}

// Recursive async fn needs boxing in stable Rust.
fn collect_files<'a>(
    base: &'a Path,
    dir: &'a Path,
    out: &'a mut Vec<ListEntry>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else { return };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let Ok(file_type) = entry.file_type().await else { continue };
            if file_type.is_dir() {
                // Backup-version slots live under .versions/ — they're not
                // restorable the normal way (ListVersions/GetVersion only),
                // so they shouldn't show up as if they were live files.
                let name = path.file_name().and_then(|n| n.to_str());
                if name == Some(".versions") || name == Some(INCOMING_DIR) {
                    continue;
                }
                collect_files(base, &path, out).await;
            } else if file_type.is_file() {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                let rel =
                    path.strip_prefix(base).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                out.push(ListEntry { path: rel, size, deleted: false });
            }
        }
    })
}

async fn handle_get(
    send: &mut iroh::endpoint::SendStream,
    sender_dir: &Path,
    path: String,
    data_dir: &Path,
    bandwidth_book: &BandwidthBook,
    quality: &ConnectionQuality,
) -> Result<()> {
    let Some(src) = safe_join(sender_dir, &path) else {
        protocol::write_frame(send, &SyncAck::err("invalid path")).await?;
        return Ok(());
    };

    match send_file(send, &src).await? {
        Some(sent) => crate::bandwidth::record(data_dir, bandwidth_book, quality, sent).await,
        None => protocol::write_frame(send, &SyncAck::err("not found")).await?,
    }
    Ok(())
}

async fn handle_list_versions(
    send: &mut iroh::endpoint::SendStream,
    sender_dir: &Path,
    path: String,
) -> Result<()> {
    let mut versions = Vec::new();
    for slot in 1..=MAX_BACKUP_VERSIONS {
        let Some(vp) = version_path(sender_dir, &path, slot) else { break };
        if let Ok(meta) = tokio::fs::metadata(&vp).await {
            let modified_unix = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            versions.push(VersionEntry { version: slot, size: meta.len(), modified_unix });
        }
    }
    protocol::write_frame(send, &ListVersionsResponse { versions }).await?;
    Ok(())
}

async fn handle_get_version(
    send: &mut iroh::endpoint::SendStream,
    sender_dir: &Path,
    path: String,
    version: u32,
    data_dir: &Path,
    bandwidth_book: &BandwidthBook,
    quality: &ConnectionQuality,
) -> Result<()> {
    let Some(src) = version_path(sender_dir, &path, version) else {
        protocol::write_frame(send, &SyncAck::err("invalid path")).await?;
        return Ok(());
    };

    match send_file(send, &src).await? {
        Some(sent) => crate::bandwidth::record(data_dir, bandwidth_book, quality, sent).await,
        None => protocol::write_frame(send, &SyncAck::err("not found")).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression test for a deleted file disappearing from the dashboard's
    // file browser entirely (reported against a live setup: delete a file
    // locally, let it cycle, then "Files from <buddy>" just no longer
    // listed it at all — not even with a History button — despite
    // rotate_versions() correctly keeping its content under .versions/).
    #[tokio::test]
    async fn list_sender_files_surfaces_deleted_but_recoverable_paths() {
        let dir = std::env::temp_dir()
            .join(format!("bb-list-sender-files-test-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(dir.join("notes")).await.unwrap();
        tokio::fs::create_dir_all(dir.join(".versions/notes")).await.unwrap();

        // `hello.txt` is still live.
        tokio::fs::write(dir.join("hello.txt"), b"hi").await.unwrap();
        // `notes/wtf.txt` was deleted — rotate_versions() moved its old
        // content to slot 1 before the delete, so only the version slot
        // remains, no live file.
        tokio::fs::write(dir.join(".versions/notes/wtf.txt.v1"), b"old content").await.unwrap();

        let files = list_sender_files(&dir).await;

        let hello = files.iter().find(|f| f.path == "hello.txt").expect("hello.txt listed");
        assert!(!hello.deleted, "live file should not be marked deleted");

        let wtf = files
            .iter()
            .find(|f| f.path == "notes/wtf.txt")
            .expect("deleted file should still be listed, not silently dropped");
        assert!(wtf.deleted, "a path with no live file but a surviving version should be marked deleted");
        assert_eq!(wtf.size, "old content".len() as u64);

        // Exactly one entry for the deleted path, not one per surviving slot.
        assert_eq!(files.iter().filter(|f| f.path == "notes/wtf.txt").count(), 1);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
