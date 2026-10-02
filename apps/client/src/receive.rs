use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::protocol::{self, ListEntry, ListResponse, SyncAck, SyncRequest};

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
/// under received/<their node id>/<path they gave us>, and List/Get only
/// ever look inside that one sender's own directory — a buddy can never
/// see or fetch what another buddy stored with us.
pub async fn handle_incoming(
    incoming: iroh::endpoint::Incoming,
    data_dir: PathBuf,
    pledge_book: PledgeBook,
) -> Result<()> {
    let connecting = incoming.accept()?;
    let conn = connecting.await?;
    let remote = conn.remote_id();
    let remote_id = remote.to_string();
    let sender_dir = data_dir.join("received").join(&remote_id);

    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => break, // peer closed the connection — normal end of a sync cycle
        };
        let data_dir = data_dir.clone();
        let sender_dir = sender_dir.clone();
        let pledge_book = pledge_book.clone();
        let remote_id = remote_id.clone();
        tokio::spawn(async move {
            if let Err(err) =
                handle_stream(send, recv, data_dir, sender_dir, remote_id.clone(), pledge_book).await
            {
                tracing::warn!(from = %remote_id, ?err, "request from buddy failed");
            }
        });
    }

    Ok(())
}

async fn handle_stream(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    data_dir: PathBuf,
    sender_dir: PathBuf,
    remote_id: String,
    pledge_book: PledgeBook,
) -> Result<()> {
    let request: SyncRequest =
        protocol::read_frame(&mut recv).await.context("failed to read request")?;

    match request {
        SyncRequest::Ping => {
            protocol::write_frame(&mut send, &SyncAck::ok()).await?;
        }
        SyncRequest::Put { path, ciphertext_len, ciphertext_sha256 } => {
            handle_put(
                &mut send,
                &mut recv,
                &sender_dir,
                &remote_id,
                &pledge_book,
                &data_dir,
                path,
                ciphertext_len,
                ciphertext_sha256,
            )
            .await?;
        }
        SyncRequest::Delete { path } => {
            handle_delete(&mut send, &sender_dir, &remote_id, &pledge_book, &data_dir, path).await?;
        }
        SyncRequest::List => {
            handle_list(&mut send, &sender_dir).await?;
        }
        SyncRequest::Get { path } => {
            handle_get(&mut send, &sender_dir, path).await?;
        }
    }

    send.stopped().await.ok();
    Ok(())
}

// Reject anything that would escape the sender's own directory — an
// absolute path or any ".." component. A malicious or buggy buddy client
// sending path: "../../etc/passwd" should get "invalid path", not a write
// outside received/<their id>/. Also reused by restore.rs, which joins
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
    path: String,
    ciphertext_len: u64,
    ciphertext_sha256: String,
) -> Result<()> {
    const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5GB per file, sanity cap
    if ciphertext_len > MAX_FILE_BYTES {
        protocol::write_frame(send, &SyncAck::err("file too large")).await?;
        return Ok(());
    }

    let Some(dest) = safe_join(sender_dir, &path) else {
        protocol::write_frame(send, &SyncAck::err("invalid path")).await?;
        return Ok(());
    };

    // If we already have a file at this path, only the *delta* against its
    // current size should count against the pledge — otherwise every
    // re-send of an unchanged-size file would eat into the budget again.
    let previous_size = tokio::fs::metadata(&dest).await.map(|m| m.len()).unwrap_or(0);

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
        let mut buf = vec![0u8; ciphertext_len as usize];
        if let Err(err) = recv.read_exact(&mut buf).await {
            // Roll back the reservation — we never actually got the bytes.
            let mut book = pledge_book.lock().unwrap();
            if let Some(state) = book.get_mut(remote_id) {
                state.received_bytes =
                    state.received_bytes.saturating_sub(ciphertext_len).saturating_add(previous_size);
            }
            return Err(err).context("failed to read file bytes");
        }

        let actual_hash = hex::encode(Sha256::digest(&buf));
        if actual_hash != ciphertext_sha256 {
            protocol::write_frame(send, &SyncAck::err("hash mismatch")).await?;
            let mut book = pledge_book.lock().unwrap();
            if let Some(state) = book.get_mut(remote_id) {
                state.received_bytes =
                    state.received_bytes.saturating_sub(ciphertext_len).saturating_add(previous_size);
            }
            return Ok(());
        }

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("failed to create directory for incoming file")?;
        }
        tokio::fs::write(&dest, &buf).await.context("failed to write incoming file")?;

        if let Err(err) = save_usage_ledger(data_dir, pledge_book).await {
            // Don't fail the exchange over a ledger write hiccup — worst
            // case we under-persist by one file's worth and a restart
            // re-allows it, which is the safe direction to err in.
            tracing::warn!(?err, "failed to persist usage ledger");
        }

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
    protocol::write_frame(send, &SyncAck::err("pledge exceeded")).await?;
    Ok(())
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
    match tokio::fs::remove_file(&target).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {} // idempotent: already gone is fine
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
    let mut files = Vec::new();
    collect_files(sender_dir, sender_dir, &mut files).await;
    protocol::write_frame(send, &ListResponse { files }).await?;
    Ok(())
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
                collect_files(base, &path, out).await;
            } else if file_type.is_file() {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                let rel =
                    path.strip_prefix(base).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                out.push(ListEntry { path: rel, size });
            }
        }
    })
}

async fn handle_get(
    send: &mut iroh::endpoint::SendStream,
    sender_dir: &Path,
    path: String,
) -> Result<()> {
    let Some(src) = safe_join(sender_dir, &path) else {
        protocol::write_frame(send, &SyncAck::err("invalid path")).await?;
        return Ok(());
    };

    match tokio::fs::read(&src).await {
        Ok(bytes) => {
            protocol::write_frame(send, &SyncAck::ok()).await?;
            send.write_all(&bytes).await.context("failed to send file bytes")?;
        }
        Err(_) => {
            protocol::write_frame(send, &SyncAck::err("not found")).await?;
        }
    }
    Ok(())
}
