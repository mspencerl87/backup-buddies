use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

// Replaces the old connectivity-test-only ALPN (backup-buddies/test/0).
// Real backup subsumes what that proved — a successful Ping or Put *is*
// the connectivity proof now, so there's no reason to run two protocols.
pub const ALPN: &[u8] = b"backup-buddies/sync/1";

// Every exchange on a stream starts with one framed SyncRequest, answered
// with one framed SyncAck (Ping/Put/Delete), a ListResponse (List), or a
// SyncAck followed by raw bytes (Get). Framing is a 4-byte little-endian
// length prefix followed by that many bytes of JSON — simple, and keeps
// control messages clearly separated from the raw file bytes that follow
// some of them on the same stream.
const MAX_FRAME_BYTES: usize = 1024 * 1024; // control messages are tiny; generous headroom

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SyncRequest {
    /// No backup work to do this cycle — just prove the connection still
    /// works. Same purpose the old test-payload protocol served.
    Ping,
    /// "Here's a new/changed file." Followed immediately on the stream by
    /// exactly `ciphertext_len` raw bytes — not framed, since the header
    /// already states the length.
    Put {
        path: String,
        ciphertext_len: u64,
        ciphertext_sha256: String,
    },
    /// "Forget this file" — frees up the sender's pledge usage with us.
    /// Idempotent: deleting something already gone is not an error.
    Delete { path: String },
    /// "What do you have backed up for me?" Answered with ListResponse.
    /// Scoped to the connecting peer's own authenticated node id — a
    /// buddy can only ever list/fetch what *they themselves* stored with
    /// us, never another buddy's data (see receive.rs).
    List,
    /// "Send me this one file's ciphertext back." Answered with a
    /// SyncAck (ok:false if not found), or on success a SyncAck{ok:true}
    /// immediately followed by the raw ciphertext bytes.
    Get { path: String },
    /// "What does your disk actually look like?" — real filesystem space
    /// plus how much of it you've committed across every buddy, not just
    /// what you've pledged me. Answered with DiskStatsResponse. Lets each
    /// side's dashboard show the other's real numbers instead of just
    /// trusting the pledge they agreed to, so an over-pledged or
    /// nearly-full buddy is visible before it becomes a failed backup.
    DiskStats,
    /// "What old copies of this file do you still have?" — every time a
    /// path is overwritten or deleted, the previous content is kept as a
    /// numbered backup rather than discarded (see receive.rs's
    /// rotate_versions). Answered with ListVersionsResponse.
    ListVersions { path: String },
    /// "Send me backup slot N of this file." Same response shape as Get:
    /// a SyncAck, then on success the raw ciphertext bytes.
    GetVersion { path: String, version: u32 },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncAck {
    pub ok: bool,
    pub error: Option<String>,
}

impl SyncAck {
    pub fn ok() -> Self {
        Self { ok: true, error: None }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self { ok: false, error: Some(msg.into()) }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListEntry {
    pub path: String,
    pub size: u64,
    /// True when this path has no live file anymore — it was deleted, and
    /// what's left is only a backup slot under `.versions/` (see
    /// `rotate_versions` in receive.rs). `size` for a deleted entry is the
    /// size of its newest surviving version, not a live file. Lets the
    /// dashboard's file browser still show (and offer History/restore-a-
    /// version for) a file the person deleted, instead of it silently
    /// disappearing from the list the moment there's no live copy.
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListResponse {
    pub files: Vec<ListEntry>,
}

/// Answer to `SyncRequest::DiskStats` — the responder's real, physical
/// filesystem numbers (not a promise): what's actually on disk under its
/// DATA_DIR, plus the sum of what it's pledged out across every buddy it
/// has (including, but not limited to, the one asking). Zeroed out rather
/// than omitted when the host couldn't read disk stats, so a buddy on an
/// unusual filesystem shows up as "unknown" instead of breaking the
/// dashboard that's asking.
#[derive(Debug, Serialize, Deserialize)]
pub struct DiskStatsResponse {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub pledged_out_total_bytes: u64,
}

/// One backed-up previous copy of a file. `version: 1` is the most
/// recently replaced/deleted content, counting up to however many backup
/// slots this host keeps (see receive.rs::MAX_BACKUP_VERSIONS) — the
/// live/current copy (if it still exists) is not included here, it's what
/// List/Get already report.
#[derive(Debug, Serialize, Deserialize)]
pub struct VersionEntry {
    pub version: u32,
    pub size: u64,
    pub modified_unix: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListVersionsResponse {
    pub versions: Vec<VersionEntry>,
}

pub async fn write_frame<T: Serialize>(send: &mut iroh::endpoint::SendStream, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).context("failed to encode frame")?;
    if bytes.len() > MAX_FRAME_BYTES {
        anyhow::bail!("frame too large to send ({} bytes)", bytes.len());
    }
    send.write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .context("failed to write frame length")?;
    send.write_all(&bytes).await.context("failed to write frame body")?;
    Ok(())
}

pub async fn read_frame<T: DeserializeOwned>(recv: &mut iroh::endpoint::RecvStream) -> Result<T> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .context("failed to read frame length")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        anyhow::bail!("peer sent an oversized frame ({len} bytes)");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.context("failed to read frame body")?;
    serde_json::from_slice(&buf).context("failed to decode frame")
}
