use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// What we last told a given buddy we'd sent them: one entry per relative
/// file path. Lets `backup.rs` diff the current state of BACKUP_DIR
/// against "what the buddy should already have" without re-sending
/// anything that hasn't changed. Hashing is still done locally every
/// cycle — there's no cheap alternative without a filesystem watcher —
/// but the network round trip and the buddy-side write are skipped for
/// anything whose hash and size already match.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Plaintext size and hash of the local file — what `backup.rs`
    /// diffs against on every cycle to decide if something changed.
    pub size: u64,
    pub sha256: String,
    /// Size of the *encrypted* bytes actually sent to the buddy and
    /// stored on their end — bigger than `size` (age's format has fixed
    /// overhead per file), and what the dashboard shows, so it's directly
    /// comparable to the buddy's own "received" byte count instead of
    /// mixing plaintext and ciphertext totals. `#[serde(default)]` so a
    /// manifest written before this field existed still loads (as 0,
    /// corrected the next time that file is re-sent).
    #[serde(default)]
    pub ciphertext_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Manifest {
    pub entries: HashMap<String, ManifestEntry>,
}

impl Manifest {
    // One manifest file per buddy, not one shared file — different buddies
    // can be pledged different amounts and may fall behind or catch up
    // independently (e.g. one's offline for a week), so "what have we told
    // *this* buddy" has to be tracked separately per buddy.
    fn path_for(data_dir: &Path, buddy_node_id: &str) -> PathBuf {
        data_dir.join(format!("manifest-{buddy_node_id}.json"))
    }

    pub async fn load(data_dir: &Path, buddy_node_id: &str) -> Self {
        match tokio::fs::read(Self::path_for(data_dir, buddy_node_id)).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub async fn save(&self, data_dir: &Path, buddy_node_id: &str) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("failed to encode manifest")?;
        tokio::fs::write(Self::path_for(data_dir, buddy_node_id), json)
            .await
            .context("failed to write manifest")?;
        Ok(())
    }
}
