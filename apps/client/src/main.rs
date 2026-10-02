mod backup;
mod dashboard;
mod manifest;
mod protocol;
mod receive;
mod restore;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use age::secrecy::SecretString;
use anyhow::{Context, Result};
use iroh::{
    endpoint::presets::Minimal, Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey,
};
use serde::Deserialize;

use receive::{PledgeBook, PledgeState};

struct Config {
    api_url: String,
    device_token: String,
    passphrase: SecretString,
    relay_url: RelayUrl,
    data_dir: PathBuf,
    // Where the real files to back up live. Optional — a client that's
    // only ever meant to receive (e.g. this machine has lots of disk and
    // is mainly someone else's buddy) doesn't need one. None here means
    // the periodic backup cycle is skipped entirely, but this device still
    // accepts and stores whatever its buddy sends it.
    backup_dir: Option<PathBuf>,
    // Local status/restore dashboard's bind address. Defaults to
    // 0.0.0.0:8080 *inside the container* — safe by default in practice
    // only because docker-compose.yml publishes it bound to 127.0.0.1 on
    // the host side; binding to loopback here instead would make it
    // unreachable through Docker's port mapping at all.
    dashboard_bind: String,
}

impl Config {
    fn from_env() -> Result<Self> {
        let api_url = std::env::var("API_URL").context("API_URL env var is required")?;
        let device_token =
            std::env::var("DEVICE_TOKEN").context("DEVICE_TOKEN env var is required")?;
        let passphrase = std::env::var("BACKUP_PASSPHRASE")
            .context("BACKUP_PASSPHRASE env var is required — this never leaves your machine")?;
        let relay_url = std::env::var("RELAY_URL")
            .unwrap_or_else(|_| "https://relay.filegarden.net".to_string())
            .parse()
            .context("RELAY_URL is not a valid URL")?;
        let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".to_string()).into();
        let backup_dir = std::env::var("BACKUP_DIR").ok().map(PathBuf::from);
        let dashboard_bind =
            std::env::var("DASHBOARD_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

        Ok(Self {
            api_url,
            device_token,
            passphrase: SecretString::from(passphrase),
            relay_url,
            data_dir,
            backup_dir,
            dashboard_bind,
        })
    }
}

#[derive(Deserialize)]
struct BuddyDevice {
    account_id: String,
    iroh_node_id: Option<String>,
    label: Option<String>,
    // node-postgres serializes BIGINT as a JSON string (to avoid precision
    // loss past 2^53), not a number — hence String here, parsed below.
    pledged_bytes: Option<String>,
}

#[derive(Deserialize)]
struct BuddiesResponse {
    buddy_devices: Vec<BuddyDevice>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let config = Config::from_env()?;

    tokio::fs::create_dir_all(&config.data_dir).await?;
    tokio::fs::create_dir_all(config.data_dir.join("received")).await?;

    let secret_key = load_or_generate_secret_key(&config.data_dir).await?;
    let endpoint_id = secret_key.public();

    let endpoint = Endpoint::builder(Minimal)
        .secret_key(secret_key)
        .alpns(vec![protocol::ALPN.to_vec()])
        .relay_mode(RelayMode::custom([config.relay_url.clone()]))
        .bind()
        .await
        .context("failed to bind Iroh endpoint")?;

    tracing::info!(node_id = %endpoint_id, "backup-buddies client starting");

    // `restore <buddy node id> [restore_dir]` pulls everything that buddy
    // is holding for us and exits — a one-shot operation, not part of the
    // normal daemon loop below. e.g.:
    //   docker compose run --rm client restore <node-id> /data/restored
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("restore") {
        let buddy_id: EndpointId = args
            .get(2)
            .context("usage: restore <buddy node id> [restore_dir]")?
            .parse()
            .context("not a valid Iroh node id")?;
        let restore_dir: PathBuf =
            args.get(3).cloned().unwrap_or_else(|| "/data/restored".to_string()).into();
        tokio::fs::create_dir_all(&restore_dir).await?;

        tracing::info!(buddy = %buddy_id, dir = %restore_dir.display(), "starting restore");
        let restored = restore::run_restore(
            &endpoint,
            &config.relay_url,
            &config.passphrase,
            buddy_id,
            &restore_dir,
        )
        .await?;
        tracing::info!(count = restored, dir = %restore_dir.display(), "restore complete");

        endpoint.close().await;
        return Ok(());
    }

    let http = reqwest::Client::new();

    register_node_id(&http, &config, &endpoint_id).await?;

    // Seed the pledge book with what we've received from each buddy so
    // far (pledged_bytes is left at 0 for now — the poll loop below fills
    // it in on its first run, before accept_task can enforce anything
    // meaningfully; until then, incoming data is rejected, which is the
    // safe default for an unknown pledge).
    let pledge_book: PledgeBook = Arc::new(Mutex::new(
        receive::load_usage_ledger(&config.data_dir)
            .await
            .into_iter()
            .map(|(id, received_bytes)| (id, PledgeState { received_bytes, ..Default::default() }))
            .collect(),
    ));

    // What the outbound loop below did on its last pass per buddy — the
    // dashboard reads this directly rather than probing anything itself.
    let cycle_stats: dashboard::CycleBook = Arc::new(Mutex::new(std::collections::HashMap::new()));

    let dashboard_task = {
        let endpoint = endpoint.clone();
        let config_dashboard_bind = config.dashboard_bind.clone();
        let node_id_str = endpoint_id.to_string();
        let data_dir = config.data_dir.clone();
        let backup_dir = config.backup_dir.clone();
        let pledge_book = pledge_book.clone();
        let cycle_stats = cycle_stats.clone();
        let relay_url = config.relay_url.clone();
        let passphrase = config.passphrase.clone();
        tokio::spawn(async move {
            if let Err(err) = dashboard::run(
                &config_dashboard_bind,
                node_id_str,
                data_dir,
                backup_dir,
                pledge_book,
                cycle_stats,
                endpoint,
                relay_url,
                passphrase,
            )
            .await
            {
                tracing::warn!(?err, "dashboard server stopped");
            }
        })
    };

    // Accept loop: our buddy's client may connect to *us* first. Runs for
    // the life of the process alongside the periodic outbound cycle below.
    let accept_task = {
        let endpoint = endpoint.clone();
        let data_dir = config.data_dir.clone();
        let pledge_book = pledge_book.clone();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let data_dir = data_dir.clone();
                let pledge_book = pledge_book.clone();
                tokio::spawn(async move {
                    if let Err(err) = receive::handle_incoming(incoming, data_dir, pledge_book).await {
                        tracing::warn!(?err, "incoming connection failed");
                    }
                });
            }
        })
    };

    // Periodic loop: ask the control plane who our paired buddies' devices
    // are, refresh their pledge, and — if BACKUP_DIR is set — push up
    // whatever's changed there since the last cycle. When there's nothing
    // to back up (no BACKUP_DIR configured), this still serves as the
    // connectivity check the old test-payload loop used to be, via Ping.
    let outbound_task = {
        let endpoint = endpoint.clone();
        let http = http.clone();
        let pledge_book = pledge_book.clone();
        let cycle_stats = cycle_stats.clone();
        async move {
            loop {
                match fetch_buddy_devices(&http, &config).await {
                    Ok(buddies) => {
                        for buddy in buddies {
                            let Some(node_id_str) = &buddy.iroh_node_id else {
                                tracing::info!(
                                    account_id = %buddy.account_id,
                                    "buddy hasn't registered a device yet — skipping"
                                );
                                continue;
                            };
                            let Ok(buddy_id) = node_id_str.parse::<EndpointId>() else {
                                tracing::warn!(node_id = %node_id_str, "buddy sent an unparseable node id");
                                continue;
                            };

                            // Refresh what this buddy has pledged us, every
                            // cycle — a re-created pairing can change it,
                            // and this is also how a brand-new buddy first
                            // gets enforcement enabled for incoming data
                            // (see receive.rs's "unknown buddy" handling).
                            let pledged_bytes = buddy
                                .pledged_bytes
                                .as_deref()
                                .and_then(|s| s.parse::<u64>().ok())
                                .unwrap_or(0);
                            {
                                let mut book = pledge_book.lock().unwrap();
                                book.entry(node_id_str.clone()).or_default().pledged_bytes = pledged_bytes;
                            }

                            let label = buddy.label.clone().unwrap_or_else(|| buddy.account_id.clone());

                            if let Some(backup_dir) = &config.backup_dir {
                                let result = backup::run_backup_cycle(
                                    &endpoint,
                                    &config.relay_url,
                                    &config.passphrase,
                                    &config.data_dir,
                                    backup_dir,
                                    buddy_id,
                                )
                                .await;
                                let record = match &result {
                                    Ok((0, 0)) => dashboard::CycleRecord {
                                        label: label.clone(),
                                        at: dashboard::unix_now(),
                                        kind: "backup",
                                        ok: true,
                                        detail: "nothing new to back up".to_string(),
                                        files_sent: Some(0),
                                        files_deleted: Some(0),
                                    },
                                    Ok((put, deleted)) => dashboard::CycleRecord {
                                        label: label.clone(),
                                        at: dashboard::unix_now(),
                                        kind: "backup",
                                        ok: true,
                                        detail: "backup cycle complete".to_string(),
                                        files_sent: Some(*put),
                                        files_deleted: Some(*deleted),
                                    },
                                    Err(err) => dashboard::CycleRecord {
                                        label: label.clone(),
                                        at: dashboard::unix_now(),
                                        kind: "backup",
                                        ok: false,
                                        detail: err.to_string(),
                                        files_sent: None,
                                        files_deleted: None,
                                    },
                                };
                                dashboard::record_cycle(&cycle_stats, node_id_str, record);
                                match result {
                                    Ok((0, 0)) => tracing::debug!(buddy = %label, "nothing new to back up"),
                                    Ok((put, deleted)) => tracing::info!(
                                        buddy = %label,
                                        files_sent = put,
                                        files_deleted = deleted,
                                        "backup cycle complete"
                                    ),
                                    Err(err) => tracing::warn!(buddy = %label, ?err, "backup cycle failed"),
                                }
                            } else {
                                let result = ping(&endpoint, &config.relay_url, buddy_id).await;
                                let record = dashboard::CycleRecord {
                                    label: label.clone(),
                                    at: dashboard::unix_now(),
                                    kind: "ping",
                                    ok: result.is_ok(),
                                    detail: match &result {
                                        Ok(()) => "connectivity check succeeded".to_string(),
                                        Err(err) => err.to_string(),
                                    },
                                    files_sent: None,
                                    files_deleted: None,
                                };
                                dashboard::record_cycle(&cycle_stats, node_id_str, record);
                                match result {
                                    Ok(()) => tracing::info!(buddy = %label, "connectivity check succeeded"),
                                    Err(err) => {
                                        tracing::warn!(buddy = %label, ?err, "connectivity check failed")
                                    }
                                }
                            }
                        }
                    }
                    Err(err) => tracing::warn!(?err, "could not fetch buddy devices from API"),
                }

                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    };

    tokio::select! {
        _ = accept_task => {}
        _ = outbound_task => {}
        _ = dashboard_task => {}
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutting down");
        }
    }

    endpoint.close().await;
    Ok(())
}

async fn load_or_generate_secret_key(data_dir: &Path) -> Result<SecretKey> {
    let key_path = data_dir.join("iroh_secret_key");
    if let Ok(bytes) = tokio::fs::read(&key_path).await {
        let array: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .context("stored iroh_secret_key file is the wrong size")?;
        return Ok(SecretKey::from_bytes(&array));
    }

    let key = SecretKey::generate();
    tokio::fs::write(&key_path, key.to_bytes()).await?;
    tracing::info!("generated a new, persistent Iroh identity for this device");
    Ok(key)
}

async fn register_node_id(http: &reqwest::Client, config: &Config, endpoint_id: &EndpointId) -> Result<()> {
    http.put(format!("{}/devices/me/node-id", config.api_url))
        .bearer_auth(&config.device_token)
        .json(&serde_json::json!({ "iroh_node_id": endpoint_id.to_string() }))
        .send()
        .await
        .context("failed to reach the API to register this device's node id")?
        .error_for_status()
        .context("API rejected device registration — check DEVICE_TOKEN")?;

    tracing::info!("registered this device's node id with the API");
    Ok(())
}

async fn fetch_buddy_devices(http: &reqwest::Client, config: &Config) -> Result<Vec<BuddyDevice>> {
    let resp: BuddiesResponse = http
        .get(format!("{}/devices/me/buddies", config.api_url))
        .bearer_auth(&config.device_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp.buddy_devices)
}

// Connectivity check for a buddy we're not backing anything up to (no
// BACKUP_DIR configured) — proves registration, discovery, NAT traversal
// via our relay, and the Iroh connection all still work, the same thing
// the old dedicated test-payload protocol proved, now just one variant of
// the real sync protocol instead of a second one running alongside it.
async fn ping(endpoint: &Endpoint, relay_url: &RelayUrl, buddy_id: EndpointId) -> Result<()> {
    let addr = EndpointAddr::new(buddy_id).with_relay_url(relay_url.clone());
    let conn = endpoint.connect(addr, protocol::ALPN).await.context("failed to connect to buddy over Iroh")?;

    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    protocol::write_frame(&mut send, &protocol::SyncRequest::Ping).await?;
    send.finish().context("failed to finish send stream")?;

    let ack: protocol::SyncAck =
        protocol::read_frame(&mut recv).await.context("failed to read ack from buddy")?;
    if !ack.ok {
        anyhow::bail!("buddy rejected ping: {}", ack.error.unwrap_or_default());
    }
    Ok(())
}
