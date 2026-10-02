use iroh::endpoint::Connection;
use serde::Serialize;

/// Which kind of network path a connection ended up using. iroh tries a
/// direct, hole-punched peer-to-peer path first and falls back to relaying
/// through relay.filegarden.net when NAT traversal doesn't work out —
/// this is how the dashboard shows which one actually happened for a
/// given backup/restore, without anyone needing to know what iroh or QUIC
/// paths are.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    Direct,
    Relay,
    /// The connection hadn't settled on a path yet when this was read —
    /// shown as "checking" rather than guessing one way or the other.
    Unknown,
}

/// A snapshot of the network path actually carrying a connection's
/// traffic right now, plus round-trip latency on that path. None of this
/// is wire protocol — it's purely local telemetry about *our* link to the
/// buddy, read straight off iroh's connection object, so it costs nothing
/// extra on the network.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ConnectionQuality {
    pub path: PathKind,
    pub rtt_ms: Option<u64>,
}

/// Reads the currently-selected path off a live connection.
pub fn connection_quality(conn: &Connection) -> ConnectionQuality {
    let paths = conn.paths();
    match paths.iter().find(|p| p.is_selected()) {
        Some(p) => ConnectionQuality {
            path: if p.is_relay() { PathKind::Relay } else { PathKind::Direct },
            rtt_ms: Some(p.rtt().as_millis() as u64),
        },
        None => ConnectionQuality { path: PathKind::Unknown, rtt_ms: None },
    }
}
