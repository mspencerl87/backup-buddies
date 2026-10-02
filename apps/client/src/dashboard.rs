use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use age::secrecy::SecretString;
use axum::{
    extract::{Path as AxumPath, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use iroh::{Endpoint, EndpointId, RelayUrl};
use serde::Serialize;

use crate::manifest::Manifest;
use crate::receive::{self, PledgeBook};

// Brand assets, shared with apps/web/assets (the canonical copy — see
// that directory's files and this crate's Dockerfile for how they get
// here). Embedded at compile time rather than served from disk: this
// dashboard has no static-file directory of its own, and two tiny SVGs
// aren't worth adding one for.
const MARK_SVG: &str = include_str!("../assets/mark.svg");
const FAVICON_SVG: &str = include_str!("../assets/favicon.svg");

/// One entry per buddy, updated by main.rs's outbound loop every time it
/// finishes a backup cycle (or a ping, for a buddy with no BACKUP_DIR) —
/// what the dashboard shows as "last cycle" is exactly what that loop just
/// did, not a separate probe.
#[derive(Clone, Serialize)]
pub struct CycleRecord {
    pub label: String,
    pub at: u64,
    pub kind: &'static str, // "backup" | "ping"
    pub ok: bool,
    pub detail: String,
    pub files_sent: Option<usize>,
    pub files_deleted: Option<usize>,
}

pub type CycleBook = Arc<Mutex<HashMap<String, CycleRecord>>>;

pub fn record_cycle(cycle_stats: &CycleBook, node_id: &str, record: CycleRecord) {
    cycle_stats.lock().unwrap().insert(node_id.to_string(), record);
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[derive(Clone)]
struct AppState {
    node_id: String,
    data_dir: PathBuf,
    backup_dir: Option<PathBuf>,
    pledge_book: PledgeBook,
    cycle_stats: CycleBook,
    endpoint: Endpoint,
    relay_url: RelayUrl,
    passphrase: SecretString,
}

#[derive(Serialize)]
struct BuddyStatus {
    node_id: String,
    label: Option<String>,
    // What they've pledged us, and how much of it we've used storing
    // their files (see receive.rs's pledge enforcement).
    pledged_bytes: u64,
    received_bytes_counted: u64,
    received_files: usize,
    received_total_bytes: u64,
    // What *we've* sent *them*, per our own manifest for that buddy.
    sent_files: usize,
    sent_bytes: u64,
    last_cycle: Option<CycleRecord>,
}

#[derive(Serialize)]
struct StatusResponse {
    node_id: String,
    backup_dir: Option<String>,
    buddies: Vec<BuddyStatus>,
}

#[derive(Serialize)]
struct RestoreResponse {
    restored_files: usize,
    dir: String,
}

/// Binds and serves the dashboard until the process shuts down. Meant to
/// run alongside the accept/outbound loops in main.rs's tokio::select!,
/// not instead of them — it only reads the same state those loops already
/// maintain (pledge_book, cycle_stats) plus whatever's on disk (manifests,
/// received/), it never competes with them for the sync protocol itself.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    bind_addr: &str,
    node_id: String,
    data_dir: PathBuf,
    backup_dir: Option<PathBuf>,
    pledge_book: PledgeBook,
    cycle_stats: CycleBook,
    endpoint: Endpoint,
    relay_url: RelayUrl,
    passphrase: SecretString,
) -> anyhow::Result<()> {
    let state = AppState {
        node_id,
        data_dir,
        backup_dir,
        pledge_book,
        cycle_stats,
        endpoint,
        relay_url,
        passphrase,
    };

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/favicon.svg", get(favicon_handler))
        .route("/mark.svg", get(mark_handler))
        .route("/api/status", get(status_handler))
        .route("/api/restore/:node_id", post(restore_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!(addr = %bind_addr, "dashboard listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index_handler() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn favicon_handler() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/svg+xml")], FAVICON_SVG)
}

async fn mark_handler() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/svg+xml")], MARK_SVG)
}

async fn status_handler(State(state): State<AppState>) -> Json<StatusResponse> {
    let pledge_snapshot = { state.pledge_book.lock().unwrap().clone() };
    let cycle_snapshot = { state.cycle_stats.lock().unwrap().clone() };

    // Union of "anyone we've received a pledge from" and "anyone we've
    // run a cycle against" — a buddy can appear in only one of the two
    // right after startup, before the other side has happened yet.
    let mut buddy_ids: BTreeSet<String> = pledge_snapshot.keys().cloned().collect();
    buddy_ids.extend(cycle_snapshot.keys().cloned());

    let mut buddies = Vec::with_capacity(buddy_ids.len());
    for node_id in buddy_ids {
        let pledge = pledge_snapshot.get(&node_id).copied().unwrap_or_default();
        let cycle = cycle_snapshot.get(&node_id).cloned();

        let manifest = Manifest::load(&state.data_dir, &node_id).await;
        let sent_files = manifest.entries.len();
        // ciphertext_size, not size (plaintext) — comparable to
        // received_total_bytes below, which is also a ciphertext size (the
        // buddy's own received/ files on disk are the encrypted bytes).
        let sent_bytes: u64 = manifest.entries.values().map(|e| e.ciphertext_size).sum();

        let sender_dir = state.data_dir.join("received").join(&node_id);
        let received = receive::list_sender_files(&sender_dir).await;
        let received_files = received.len();
        let received_total_bytes: u64 = received.iter().map(|f| f.size).sum();

        buddies.push(BuddyStatus {
            label: cycle.as_ref().map(|c| c.label.clone()),
            node_id,
            pledged_bytes: pledge.pledged_bytes,
            received_bytes_counted: pledge.received_bytes,
            received_files,
            received_total_bytes,
            sent_files,
            sent_bytes,
            last_cycle: cycle,
        });
    }

    Json(StatusResponse {
        node_id: state.node_id.clone(),
        backup_dir: state.backup_dir.as_ref().map(|p| p.display().to_string()),
        buddies,
    })
}

// Pulls everything that buddy currently has stored for us and writes it
// under data_dir/restored/<their node id>/ — a fixed, predictable
// location (separate per buddy, so restoring from two buddies in a row
// can't clobber each other), unlike the `restore` CLI subcommand which
// takes an explicit destination. Same underlying restore::run_restore.
async fn restore_handler(
    State(state): State<AppState>,
    AxumPath(node_id_str): AxumPath<String>,
) -> impl IntoResponse {
    let buddy_id: EndpointId = match node_id_str.parse() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "not a valid Iroh node id".to_string()).into_response(),
    };

    let restore_dir = state.data_dir.join("restored").join(&node_id_str);
    if let Err(err) = tokio::fs::create_dir_all(&restore_dir).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("couldn't create restore directory: {err}"))
            .into_response();
    }

    match crate::restore::run_restore(
        &state.endpoint,
        &state.relay_url,
        &state.passphrase,
        buddy_id,
        &restore_dir,
    )
    .await
    {
        Ok(restored_files) => {
            Json(RestoreResponse { restored_files, dir: restore_dir.display().to_string() })
                .into_response()
        }
        Err(err) => (StatusCode::BAD_GATEWAY, format!("restore failed: {err}")).into_response(),
    }
}

// A single self-contained page — no build step, no bundler, consistent
// with how small this client otherwise is. Polls /api/status every 5s and
// re-renders; the "Restore now" button posts to /api/restore/<id> and
// shows the result (or error) inline rather than navigating away.
const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Backup Buddies — this device</title>
<link rel="icon" type="image/svg+xml" href="/favicon.svg">
<style>
  :root {
    --bg: #0f1115; --surface: #171a21; --border: #2a2e38; --text: #e8e9ec;
    --text-muted: #9499a6; --accent: #4f8cff; --accent-text: #0f1115;
    --danger: #ff6b6b; --ok: #4caf7d; --radius: 10px;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
    background: var(--bg); color: var(--text); line-height: 1.5;
  }
  header { padding: 1.25rem 1.5rem; border-bottom: 1px solid var(--border); }
  header .title-row { display: flex; align-items: center; gap: 0.6rem; }
  header .title-row img { width: 26px; height: 26px; display: block; }
  header h1 { font-size: 1.1rem; margin: 0; }
  header .sub { color: var(--text-muted); font-size: 0.85rem; margin-top: 0.25rem; }
  main { max-width: 920px; margin: 0 auto; padding: 1.5rem; }
  .card {
    background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius);
    padding: 1.25rem 1.5rem; margin-bottom: 1.25rem;
  }
  .card h2 { margin: 0 0 0.75rem; font-size: 1rem; }
  .row { display: flex; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
  .pill {
    display: inline-block; padding: 0.15rem 0.55rem; border-radius: 999px;
    font-size: 0.75rem; font-weight: 600;
  }
  .pill-ok { background: rgba(76,175,125,0.15); color: var(--ok); }
  .pill-err { background: rgba(255,107,107,0.15); color: var(--danger); }
  .pill-pending { background: rgba(148,153,166,0.15); color: var(--text-muted); }
  .muted { color: var(--text-muted); font-size: 0.85rem; }
  .bar-track { background: #11141a; border-radius: 6px; height: 6px; overflow: hidden; margin-top: 0.4rem; }
  .bar-fill { background: var(--accent); height: 100%; }
  button {
    background: var(--accent); color: var(--accent-text); border: none; border-radius: 8px;
    padding: 0.45rem 0.9rem; font-size: 0.85rem; font-weight: 600; cursor: pointer;
  }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  .restore-result { font-size: 0.8rem; margin-top: 0.5rem; }
  code { background: #11141a; padding: 0.1rem 0.35rem; border-radius: 4px; font-size: 0.85em; }
  #empty { color: var(--text-muted); }
</style>
</head>
<body>
<header>
  <div class="title-row">
    <img src="/mark.svg" alt="">
    <h1>Backup Buddies</h1>
  </div>
  <div class="sub">This device: <code id="node-id">…</code> · backing up <code id="backup-dir">…</code></div>
</header>
<main>
  <div id="empty" class="card" style="display:none;">No buddies seen yet — waiting on the first poll of the API.</div>
  <div id="buddies"></div>
</main>
<script>
function fmtBytes(n) {
  if (n === 0) return "0 B";
  const u = ["B","KB","MB","GB","TB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return (n / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1) + " " + u[i];
}
function fmtAgo(unixSecs) {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - unixSecs);
  if (secs < 60) return secs + "s ago";
  if (secs < 3600) return Math.floor(secs / 60) + "m ago";
  return Math.floor(secs / 3600) + "h ago";
}

async function restoreNow(nodeId, btn, resultEl) {
  btn.disabled = true;
  resultEl.textContent = "Restoring…";
  try {
    const res = await fetch("/api/restore/" + encodeURIComponent(nodeId), { method: "POST" });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(body.error || ("HTTP " + res.status));
    resultEl.textContent = "Restored " + body.restored_files + " file(s) to " + body.dir;
    resultEl.style.color = "var(--ok)";
  } catch (err) {
    resultEl.textContent = "Restore failed: " + err.message;
    resultEl.style.color = "var(--danger)";
  } finally {
    btn.disabled = false;
  }
}

function renderBuddy(b) {
  const pct = b.pledged_bytes > 0 ? Math.min(100, (b.received_bytes_counted / b.pledged_bytes) * 100) : 0;
  const cycle = b.last_cycle;
  let cyclePill = '<span class="pill pill-pending">no cycle yet</span>';
  let cycleDetail = "";
  if (cycle) {
    cyclePill = cycle.ok
      ? '<span class="pill pill-ok">' + cycle.kind + ' ok</span>'
      : '<span class="pill pill-err">' + cycle.kind + ' failed</span>';
    cycleDetail = cycle.detail + " · " + fmtAgo(cycle.at);
    if (cycle.kind === "backup" && cycle.ok) {
      cycleDetail = "sent " + (cycle.files_sent ?? 0) + ", deleted " + (cycle.files_deleted ?? 0) + " · " + fmtAgo(cycle.at);
    }
  }

  const div = document.createElement("div");
  div.className = "card";
  div.innerHTML = `
    <div class="row">
      <h2>${b.label ? b.label : b.node_id.slice(0, 12) + "…"}</h2>
      ${cyclePill}
    </div>
    <div class="muted">${cycleDetail}</div>
    <div class="row" style="margin-top: 0.9rem;">
      <div>
        <div class="muted">You've sent them</div>
        <div>${b.sent_files} file(s), ${fmtBytes(b.sent_bytes)}</div>
      </div>
      <div>
        <div class="muted">They've sent you</div>
        <div>${b.received_files} file(s), ${fmtBytes(b.received_total_bytes)}</div>
      </div>
    </div>
    <div style="margin-top: 0.75rem;">
      <div class="muted">Their pledge to you: ${fmtBytes(b.received_bytes_counted)} / ${fmtBytes(b.pledged_bytes)}</div>
      <div class="bar-track"><div class="bar-fill" style="width:${pct}%"></div></div>
    </div>
    <div style="margin-top: 1rem;">
      <button data-node-id="${b.node_id}">Restore from this buddy</button>
      <div class="restore-result"></div>
    </div>
  `;
  const btn = div.querySelector("button");
  const resultEl = div.querySelector(".restore-result");
  btn.addEventListener("click", () => restoreNow(b.node_id, btn, resultEl));
  return div;
}

async function refresh() {
  const res = await fetch("/api/status");
  const data = await res.json();
  document.getElementById("node-id").textContent = data.node_id;
  document.getElementById("backup-dir").textContent = data.backup_dir || "(nothing — receive-only)";

  const container = document.getElementById("buddies");
  const empty = document.getElementById("empty");
  if (data.buddies.length === 0) {
    empty.style.display = "block";
    container.innerHTML = "";
    return;
  }
  empty.style.display = "none";
  container.innerHTML = "";
  for (const b of data.buddies) container.appendChild(renderBuddy(b));
}

refresh();
setInterval(refresh, 5000);
</script>
</body>
</html>
"#;
