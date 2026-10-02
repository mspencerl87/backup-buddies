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
use serde::{Deserialize, Serialize};

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

/// Request body for POST /api/restore/:node_id. `paths` absent or empty
/// means "restore everything" (what the plain "Restore everything" button
/// sends) — the file browser sends specific paths once the person has
/// picked them.
#[derive(Deserialize, Default)]
struct RestoreRequest {
    #[serde(default)]
    paths: Vec<String>,
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
        .route("/api/buddies/:node_id/files", get(files_handler))
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

// Connects to the buddy and asks what they're holding for us — live, over
// the sync protocol, not cached — so the file browser always reflects
// what's actually restorable right now. A buddy with thousands of files
// means a bigger response, not a slower UI: the browser does the
// search/filter client-side over the whole list rather than paging
// through the API, which is simpler and plenty fast at this scale.
async fn files_handler(
    State(state): State<AppState>,
    AxumPath(node_id_str): AxumPath<String>,
) -> impl IntoResponse {
    let buddy_id: EndpointId = match node_id_str.parse() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "not a valid Iroh node id".to_string()).into_response(),
    };

    match crate::restore::list_buddy_files(&state.endpoint, &state.relay_url, buddy_id).await {
        Ok(files) => Json(serde_json::json!({ "files": files })).into_response(),
        Err(err) => {
            (StatusCode::BAD_GATEWAY, format!("couldn't reach buddy to list files: {err}")).into_response()
        }
    }
}

// Pulls files that buddy currently has stored for us and writes them
// under data_dir/restored/<their node id>/ — a fixed, predictable
// location (separate per buddy, so restoring from two buddies in a row
// can't clobber each other), unlike the `restore` CLI subcommand which
// takes an explicit destination. An empty/absent `paths` in the request
// body restores everything (restore::run_restore); specific paths (from
// the file browser) restore just those (restore::restore_selected).
async fn restore_handler(
    State(state): State<AppState>,
    AxumPath(node_id_str): AxumPath<String>,
    body: Option<Json<RestoreRequest>>,
) -> impl IntoResponse {
    let buddy_id: EndpointId = match node_id_str.parse() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "not a valid Iroh node id".to_string()).into_response(),
    };
    let paths = body.map(|Json(b)| b.paths).unwrap_or_default();

    let restore_dir = state.data_dir.join("restored").join(&node_id_str);
    if let Err(err) = tokio::fs::create_dir_all(&restore_dir).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("couldn't create restore directory: {err}"))
            .into_response();
    }

    let result = if paths.is_empty() {
        crate::restore::run_restore(&state.endpoint, &state.relay_url, &state.passphrase, buddy_id, &restore_dir)
            .await
    } else {
        crate::restore::restore_selected(
            &state.endpoint,
            &state.relay_url,
            &state.passphrase,
            buddy_id,
            &restore_dir,
            &paths,
        )
        .await
    };

    match result {
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
  button.secondary { background: var(--surface); color: var(--text); border: 1px solid var(--border); }
  .modal-backdrop {
    display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.6);
    align-items: center; justify-content: center; padding: 2rem; z-index: 10;
  }
  .modal-backdrop.open { display: flex; }
  .modal {
    background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius);
    width: 100%; max-width: 700px; max-height: 85vh; display: flex; flex-direction: column;
    padding: 1.25rem 1.5rem;
  }
  .modal-header { display: flex; justify-content: space-between; align-items: center; gap: 1rem; }
  .modal-header h2 { margin: 0; font-size: 1rem; }
  .modal-header button { padding: 0.25rem 0.6rem; }
  .modal-search { margin-top: 0.9rem; }
  .modal-search input {
    width: 100%; background: #11141a; border: 1px solid var(--border); border-radius: 8px;
    color: var(--text); padding: 0.5rem 0.7rem; font-size: 0.85rem;
  }
  .modal-actions { display: flex; align-items: center; justify-content: space-between; gap: 1rem; margin-top: 0.75rem; }
  .modal-actions .muted { font-size: 0.78rem; }
  .file-list { overflow-y: auto; margin-top: 0.75rem; border-top: 1px solid var(--border); }
  .file-row {
    display: flex; align-items: center; gap: 0.6rem; padding: 0.5rem 0.1rem;
    border-bottom: 1px solid var(--border); font-size: 0.82rem;
  }
  .file-row .path { flex: 1; word-break: break-all; }
  .file-row .size { color: var(--text-muted); white-space: nowrap; }
  .file-row button { padding: 0.25rem 0.6rem; font-size: 0.75rem; }
  .modal-status { font-size: 0.8rem; margin-top: 0.6rem; }
  .modal-footer { margin-top: 0.75rem; padding-top: 0.75rem; border-top: 1px solid var(--border); }
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

<div class="modal-backdrop" id="browse-modal">
  <div class="modal">
    <div class="modal-header">
      <h2 id="browse-title">Files</h2>
      <button class="secondary" id="browse-close">Close</button>
    </div>
    <div class="modal-search">
      <input type="text" id="browse-search" placeholder="Search by path…">
    </div>
    <div class="modal-actions">
      <label class="muted"><input type="checkbox" id="browse-select-all"> select all</label>
      <span class="muted" id="browse-count"></span>
    </div>
    <div class="file-list" id="browse-list"></div>
    <div class="modal-footer">
      <button id="browse-restore-selected" disabled>Restore selected</button>
      <button class="secondary" id="browse-restore-all">Restore all</button>
      <div class="restore-result" id="browse-result"></div>
    </div>
  </div>
</div>

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

async function doRestore(nodeId, paths) {
  const res = await fetch("/api/restore/" + encodeURIComponent(nodeId), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ paths: paths || [] }),
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || ("HTTP " + res.status));
  return body;
}

async function restoreNow(nodeId, btn, resultEl) {
  btn.disabled = true;
  resultEl.textContent = "Restoring…";
  try {
    const body = await doRestore(nodeId, []);
    resultEl.textContent = "Restored " + body.restored_files + " file(s) to " + body.dir;
    resultEl.style.color = "var(--ok)";
  } catch (err) {
    resultEl.textContent = "Restore failed: " + err.message;
    resultEl.style.color = "var(--danger)";
  } finally {
    btn.disabled = false;
  }
}

// --- File browser modal ---------------------------------------------------
// Fetches a buddy's file list once when opened and does all search/filter
// and selection client-side — simplest approach, and plenty fast even at a
// few thousand rows, which is the scale this is built for.
const browseState = { nodeId: null, label: "", files: [], selected: new Set() };

const browseModal = document.getElementById("browse-modal");
const browseTitle = document.getElementById("browse-title");
const browseSearch = document.getElementById("browse-search");
const browseList = document.getElementById("browse-list");
const browseCount = document.getElementById("browse-count");
const browseSelectAll = document.getElementById("browse-select-all");
const browseRestoreSelected = document.getElementById("browse-restore-selected");
const browseRestoreAll = document.getElementById("browse-restore-all");
const browseResult = document.getElementById("browse-result");

function closeBrowseModal() {
  browseModal.classList.remove("open");
  browseState.nodeId = null;
  browseState.files = [];
  browseState.selected.clear();
}

function renderBrowseList() {
  const query = browseSearch.value.trim().toLowerCase();
  const filtered = query
    ? browseState.files.filter((f) => f.path.toLowerCase().includes(query))
    : browseState.files;

  browseCount.textContent = filtered.length + " of " + browseState.files.length + " file(s)";
  browseList.innerHTML = "";

  for (const f of filtered) {
    const row = document.createElement("div");
    row.className = "file-row";
    const checked = browseState.selected.has(f.path) ? "checked" : "";
    row.innerHTML = `
      <input type="checkbox" class="file-pick" ${checked}>
      <span class="path">${f.path}</span>
      <span class="size">${fmtBytes(f.size)}</span>
      <button class="secondary file-restore-one">Restore</button>
    `;
    const checkbox = row.querySelector(".file-pick");
    checkbox.addEventListener("change", () => {
      if (checkbox.checked) browseState.selected.add(f.path);
      else browseState.selected.delete(f.path);
      updateSelectedControls();
    });
    row.querySelector(".file-restore-one").addEventListener("click", async (ev) => {
      const btn = ev.target;
      btn.disabled = true;
      browseResult.textContent = "Restoring " + f.path + "…";
      browseResult.style.color = "";
      try {
        const body = await doRestore(browseState.nodeId, [f.path]);
        browseResult.textContent = "Restored " + body.restored_files + " file(s) to " + body.dir;
        browseResult.style.color = "var(--ok)";
      } catch (err) {
        browseResult.textContent = "Restore failed: " + err.message;
        browseResult.style.color = "var(--danger)";
      } finally {
        btn.disabled = false;
      }
    });
    browseList.appendChild(row);
  }

  browseSelectAll.checked = filtered.length > 0 && filtered.every((f) => browseState.selected.has(f.path));
  updateSelectedControls();
}

function updateSelectedControls() {
  browseRestoreSelected.disabled = browseState.selected.size === 0;
  browseRestoreSelected.textContent =
    browseState.selected.size > 0 ? "Restore selected (" + browseState.selected.size + ")" : "Restore selected";
}

async function openBrowseModal(nodeId, label) {
  browseState.nodeId = nodeId;
  browseState.label = label;
  browseState.files = [];
  browseState.selected.clear();
  browseSearch.value = "";
  browseResult.textContent = "";
  browseTitle.textContent = "Files from " + label;
  browseCount.textContent = "";
  browseList.innerHTML = "";
  browseModal.classList.add("open");

  try {
    const res = await fetch("/api/buddies/" + encodeURIComponent(nodeId) + "/files");
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(body.error || ("HTTP " + res.status));
    browseState.files = (body.files || []).slice().sort((a, b) => a.path.localeCompare(b.path));
    renderBrowseList();
  } catch (err) {
    browseList.innerHTML = "";
    browseCount.textContent = "";
    browseResult.textContent = "Couldn't list files: " + err.message;
    browseResult.style.color = "var(--danger)";
  }
}

browseSearch.addEventListener("input", renderBrowseList);
document.getElementById("browse-close").addEventListener("click", closeBrowseModal);
browseModal.addEventListener("click", (ev) => {
  if (ev.target === browseModal) closeBrowseModal();
});

browseSelectAll.addEventListener("change", () => {
  const query = browseSearch.value.trim().toLowerCase();
  const filtered = query
    ? browseState.files.filter((f) => f.path.toLowerCase().includes(query))
    : browseState.files;
  if (browseSelectAll.checked) {
    for (const f of filtered) browseState.selected.add(f.path);
  } else {
    for (const f of filtered) browseState.selected.delete(f.path);
  }
  renderBrowseList();
});

browseRestoreSelected.addEventListener("click", async () => {
  const paths = Array.from(browseState.selected);
  if (paths.length === 0) return;
  browseRestoreSelected.disabled = true;
  browseResult.textContent = "Restoring " + paths.length + " file(s)…";
  browseResult.style.color = "";
  try {
    const body = await doRestore(browseState.nodeId, paths);
    browseResult.textContent = "Restored " + body.restored_files + " file(s) to " + body.dir;
    browseResult.style.color = "var(--ok)";
  } catch (err) {
    browseResult.textContent = "Restore failed: " + err.message;
    browseResult.style.color = "var(--danger)";
  } finally {
    browseRestoreSelected.disabled = browseState.selected.size === 0;
  }
});

browseRestoreAll.addEventListener("click", async () => {
  browseRestoreAll.disabled = true;
  browseResult.textContent = "Restoring everything…";
  browseResult.style.color = "";
  try {
    const body = await doRestore(browseState.nodeId, []);
    browseResult.textContent = "Restored " + body.restored_files + " file(s) to " + body.dir;
    browseResult.style.color = "var(--ok)";
  } catch (err) {
    browseResult.textContent = "Restore failed: " + err.message;
    browseResult.style.color = "var(--danger)";
  } finally {
    browseRestoreAll.disabled = false;
  }
});

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
      <button class="restore-btn" data-node-id="${b.node_id}">Restore from this buddy</button>
      <button class="secondary browse-btn" data-node-id="${b.node_id}">Browse files…</button>
      <div class="restore-result"></div>
    </div>
  `;
  const btn = div.querySelector(".restore-btn");
  const resultEl = div.querySelector(".restore-result");
  btn.addEventListener("click", () => restoreNow(b.node_id, btn, resultEl));
  const browseBtn = div.querySelector(".browse-btn");
  const label = b.label ? b.label : b.node_id.slice(0, 12) + "…";
  browseBtn.addEventListener("click", () => openBrowseModal(b.node_id, label));
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
