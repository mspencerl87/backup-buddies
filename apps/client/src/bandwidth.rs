use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::netinfo::{ConnectionQuality, PathKind};

// Lifetime, cumulative byte counts, split by which network path actually
// carried them — not per-buddy, since what matters here is "how much of
// this device's traffic went over relay.filegarden.net" (the thing that
// would actually cost something to meter/bill), not who it was with.
// Covers every data-moving exchange this device takes part in, either
// direction: our own backup pushes and restore pulls, and a buddy pushing
// to us or pulling from us. Control traffic (Ping, List, DiskStats,
// Delete) is negligible and isn't counted.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct BandwidthTotals {
    pub relay_bytes: u64,
    pub direct_bytes: u64,
}

pub type BandwidthBook = Arc<Mutex<BandwidthTotals>>;

pub async fn load(data_dir: &Path) -> BandwidthTotals {
    match tokio::fs::read(data_dir.join("bandwidth.json")).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => BandwidthTotals::default(),
    }
}

pub async fn save(data_dir: &Path, book: &BandwidthBook) -> Result<()> {
    let snapshot = *book.lock().unwrap();
    let json = serde_json::to_vec(&snapshot)?;
    tokio::fs::write(data_dir.join("bandwidth.json"), json).await?;
    Ok(())
}

/// Adds `bytes` to the running total for whichever path carried them, then
/// persists the new total. `Unknown` (a connection whose path hadn't
/// settled when read) is dropped rather than guessed into either bucket —
/// undercounting slightly is better than misattributing relay-billable
/// bytes as free direct ones, or vice versa.
pub async fn record(data_dir: &Path, book: &BandwidthBook, quality: &ConnectionQuality, bytes: u64) {
    if bytes == 0 {
        return;
    }
    {
        let mut totals = book.lock().unwrap();
        match quality.path {
            PathKind::Relay => totals.relay_bytes += bytes,
            PathKind::Direct => totals.direct_bytes += bytes,
            PathKind::Unknown => return,
        }
    }
    if let Err(err) = save(data_dir, book).await {
        tracing::warn!(?err, "failed to persist bandwidth totals");
    }
}
