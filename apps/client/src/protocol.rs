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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListResponse {
    pub files: Vec<ListEntry>,
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
