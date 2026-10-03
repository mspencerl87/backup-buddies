mod backup;
mod bandwidth;
mod dashboard;
mod manifest;
mod netinfo;
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
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
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
    // Local status/restore dashboard's bind address. Built from
    // DASHBOARD_PORT (0.0.0.0:<that port>) rather than a hardcoded 8080 —
    // with network_mode: host (see docker-compose.yml) there's no Docker
    // port-mapping layer to translate a fixed internal port to a chosen
    // host one, so the app itself has to bind the real port directly.
    // This matters for running more than one device's client on the same
    // host (see DASHBOARD_PORT's own doc comment below): each needs a
    // distinct DASHBOARD_PORT, same requirement as SYNC_PORT already has.
    // DASHBOARD_BIND remains as an escape hatch to override the full
    // address (e.g. to bind loopback-only) when DASHBOARD_PORT alone
    // isn't enough.
    dashboard_bind: String,
    // How long since a buddy's last *successful* cycle before the
    // dashboard calls them stale rather than just "last synced a while
    // ago" — see dashboard.rs's stale computation. 30s between cycles
    // means a handful of transient failures shouldn't trip this; it's
    // meant to catch a buddy that's actually stuck.
    stale_after_secs: u64,
    // The UDP port iroh binds for the actual sync traffic (separate from
    // DASHBOARD_BIND's TCP port above). Fixed and configurable, rather
    // than the random free port iroh picks by default, specifically so
    // docker-compose.yml can publish it — see the `ports:` entry there.
    // Without a published UDP port, NAT means this device can never
    // receive an inbound hole-punch attempt, so direct (peer-to-peer)
    // connections are impossible and every byte relays, even between two
    // buddies who'd otherwise connect directly fine. Forwarding this port
    // on a home router is still optional — nothing breaks without it,
    // every transfer just goes through the relay instead, same as before
    // this existed.
    sync_port: u16,
}

impl Config {
    fn from_env() -> Result<Self> {
        let api_url = std::env::var("API_URL").context("API_URL env var is required")?;
        let device_token =
            std::env::var("DEVICE_TOKEN").context("DEVICE_TOKEN env var is required")?;
        let passphrase = std::env::var("BACKUP_PASSPHRASE")
            .context("BACKUP_PASSPHRASE env var is required — this never leaves your machine")?;
        if passphrase.len() < 12 {
            tracing::warn!(
                "BACKUP_PASSPHRASE is under 12 characters — a short passphrase is much easier to \
                 guess, and there's no way to recover your buddy's copy of your data if it's ever \
                 lost or cracked (there's no \"forgot passphrase\" flow by design)"
            );
        }
        let relay_url = std::env::var("RELAY_URL")
            .unwrap_or_else(|_| "https://relay.filegarden.net".to_string())
            .parse()
            .context("RELAY_URL is not a valid URL")?;
        let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".to_string()).into();
        let backup_dir = std::env::var("BACKUP_DIR").ok().map(PathBuf::from);
        let dashboard_port = std::env::var("DASHBOARD_PORT")
            .ok()
            .map(|s| s.parse::<u16>().context("DASHBOARD_PORT must be a valid port number (1-65535)"))
            .transpose()?
            .unwrap_or(8080);
        let dashboard_bind = std::env::var("DASHBOARD_BIND")
            .unwrap_or_else(|_| format!("0.0.0.0:{dashboard_port}"));
        let stale_after_secs = std::env::var("STALE_AFTER_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600);
        let sync_port = std::env::var("SYNC_PORT")
            .ok()
            .map(|s| s.parse().context("SYNC_PORT must be a valid port number (1-65535)"))
            .transpose()?
            .unwrap_or(11235);

        Ok(Self {
            api_url,
            device_token,
            passphrase: SecretString::from(passphrase),
            relay_url,
            data_dir,
            backup_dir,
            dashboard_bind,
            stale_after_secs,
            sync_port,
        })
    }

    // Fails loud and early, before any of the long-running loops start,
    // rather than letting a bad config surface later as a confusing
    // runtime symptom — an empty-looking backup cycle because BACKUP_DIR
    // doesn't actually exist, or a cryptic connection error because
    // API_URL has a typo in it.
    async fn validate(&self) -> Result<()> {
        if let Some(backup_dir) = &self.backup_dir {
            let meta = tokio::fs::metadata(backup_dir).await.with_context(|| {
                format!(
                    "BACKUP_DIR is set to {} but it doesn't exist or isn't readable from inside the container \
                     — check the BACKUP_DIR_HOST bind mount in docker-compose.yml",
                    backup_dir.display()
                )
            })?;
            if !meta.is_dir() {
                anyhow::bail!("BACKUP_DIR ({}) exists but isn't a directory", backup_dir.display());
            }
        }

        reqwest::Url::parse(&self.api_url)
            .with_context(|| format!("API_URL ({}) isn't a valid URL", self.api_url))?;

        // DATA_DIR itself is create_dir_all'd right after this runs (which
        // already fails loudly on its own if that's not possible) — this
        // additionally proves the directory is actually *writable*, not
        // just creatable, by round-tripping a small probe file. A
        // read-only bind mount would otherwise only surface much later, as
        // a failed iroh_secret_key write.
        tokio::fs::create_dir_all(&self.data_dir)
            .await
            .with_context(|| format!("can't create DATA_DIR ({})", self.data_dir.display()))?;
        let probe = self.data_dir.join(".write-test");
        tokio::fs::write(&probe, b"ok")
            .await
            .with_context(|| format!("DATA_DIR ({}) isn't writable", self.data_dir.display()))?;
        let _ = tokio::fs::remove_file(&probe).await;

        Ok(())
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
    // Set by the API when this account's relay syncing has been paused
    // (today: a Stripe payment that failed after retries were exhausted —
    // see apps/api/src/routes/stripe-webhook.js). `#[serde(default)]` so an
    // older API that doesn't send this field yet still deserializes fine,
    // defaulting to "not paused" rather than failing the whole poll.
    #[serde(default)]
    paused: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Default to info for this crate's own logs, but quiet down
    // swarm-discovery's own chatty internal logging (e.g. "no addresses
    // for peer, not announcing" during normal startup, before this
    // device's own address is resolved yet) — it's noise, not signal,
    // for anyone reading these logs. RUST_LOG, if set, overrides this
    // entirely rather than layering on top of it.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(
            |_| tracing_subscriber::EnvFilter::new("info,swarm_discovery=warn"),
        ))
        .init();
    let config = Config::from_env()?;
    config.validate().await?;

    tokio::fs::create_dir_all(&config.data_dir).await?;
    tokio::fs::create_dir_all(config.data_dir.join("received")).await?;

    let secret_key = load_or_generate_secret_key(&config.data_dir).await?;
    let endpoint_id = secret_key.public();

    // Bind to a fixed, known port on both IP families instead of the
    // random free port iroh picks by default — clear_ip_transports()
    // first, since the builder otherwise already has its own default
    // 0.0.0.0:0 / [::]:0 binds queued up, which would leave this
    // listening on *two* sockets per family rather than replacing them.
    // See sync_port's doc comment on Config for why a fixed port matters:
    // docker-compose.yml can only publish a UDP port it knows ahead of
    // time, and a published port is what makes direct connections
    // possible at all.
    // Same idea as Syncthing's local discovery: broadcast/listen on the LAN
    // (mDNS, via swarm-discovery) so two buddies on the same local network
    // — two NAS boxes in the same house, say — find each other's private
    // address and connect directly over the LAN, never touching the relay
    // or caring about the router's NAT/hairpin behavior at all. Built
    // up-front (rather than via the builder's `.address_lookup()` taking
    // just a builder type) so we keep a handle to subscribe to its
    // discovery events below, for visibility in the logs.
    let mdns = MdnsAddressLookup::builder()
        .build(endpoint_id)
        .context("failed to start local-network (mDNS) address lookup")?;

    let endpoint = Endpoint::builder(Minimal)
        .secret_key(secret_key)
        .alpns(vec![protocol::ALPN.to_vec()])
        .relay_mode(RelayMode::custom([config.relay_url.clone()]))
        .clear_ip_transports()
        .bind_addr(format!("0.0.0.0:{}", config.sync_port))
        .context("failed to configure IPv4 sync port")?
        // is_required(false): iroh's own *default* IPv6 bind is allowed to
        // fail (plenty of hosts/containers don't have IPv6 at all), but
        // bind_addr_with_opts defaults is_required to true — without this
        // override, a container with no IPv6 would fail to start
        // entirely over a socket this code doesn't actually need.
        .bind_addr_with_opts(
            format!("[::]:{}", config.sync_port),
            iroh::endpoint::BindOpts::default().set_is_required(false),
        )
        .context("failed to configure IPv6 sync port")?
        // Purely additive: if a buddy isn't on the same LAN (the normal
        // case), mDNS finds nothing and we fall back to the
        // relay/direct-over-internet path exactly as before.
        .address_lookup(mdns.clone())
        .bind()
        .await
        .context("failed to bind Iroh endpoint")?;

    tracing::info!(node_id = %endpoint_id, "backup-buddies client starting");

    // Log what mDNS actually sees, so this is visible/debuggable from the
    // container logs rather than a silent background mechanism — doesn't
    // affect connection behavior either way, iroh uses the address lookup
    // service's results on its own regardless of whether anyone's
    // listening on this stream.
    //
    // swarm-discovery re-announces/refreshes a peer's record periodically
    // (every second or so), which re-fires Discovered for the *same*
    // peer repeatedly — logging every one of those at INFO would drown
    // out everything else in the logs within seconds. Only the first
    // sighting of a given endpoint_id (or a re-sighting after it
    // expired) is worth a log line; track what's already been announced
    // and log the rest at DEBUG instead.
    {
        let mut events = mdns.subscribe().await;
        tokio::spawn(async move {
            use n0_future::StreamExt;
            let mut currently_visible: std::collections::HashSet<EndpointId> =
                std::collections::HashSet::new();
            while let Some(event) = events.next().await {
                match event {
                    DiscoveryEvent::Discovered { endpoint_info, .. } => {
                        if currently_visible.insert(endpoint_info.endpoint_id) {
                            tracing::info!(
                                endpoint_id = %endpoint_info.endpoint_id,
                                "found a buddy on the local network (mDNS) — direct LAN connection possible"
                            );
                        } else {
                            tracing::debug!(
                                endpoint_id = %endpoint_info.endpoint_id,
                                "mDNS re-announced a buddy already known on the local network"
                            );
                        }
                    }
                    DiscoveryEvent::Expired { endpoint_id } => {
                        currently_visible.remove(&endpoint_id);
                        tracing::debug!(%endpoint_id, "mDNS-discovered buddy no longer visible on the local network");
                    }
                    // DiscoveryEvent is #[non_exhaustive] — future variants
                    // just get ignored here rather than breaking the build.
                    _ => {}
                }
            }
        });
    }

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
        let stats = restore::run_restore(
            &endpoint,
            &config.relay_url,
            &config.passphrase,
            buddy_id,
            &restore_dir,
        )
        .await?;
        tracing::info!(
            count = stats.files_restored,
            bytes = stats.bytes_restored,
            duration_ms = stats.duration_ms,
            dir = %restore_dir.display(),
            "restore complete"
        );

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

    // Lifetime relay-vs-direct byte totals — see bandwidth.rs. Loaded once
    // at startup so a restart doesn't reset the counter back to zero.
    let bandwidth_book: bandwidth::BandwidthBook =
        Arc::new(Mutex::new(bandwidth::load(&config.data_dir).await));

    // What the outbound loop below did on its last pass per buddy — the
    // dashboard reads this directly rather than probing anything itself.
    let cycle_stats: dashboard::CycleBook = Arc::new(Mutex::new(std::collections::HashMap::new()));

    // Same idea, for reconciliation results (see
    // backup::reconcile_with_buddy) — a separate book since it updates on
    // its own, much less frequent cadence, not every ordinary cycle.
    let reconcile_stats: dashboard::ReconcileBook =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    let dashboard_task = {
        let endpoint = endpoint.clone();
        let config_dashboard_bind = config.dashboard_bind.clone();
        let node_id_str = endpoint_id.to_string();
        let data_dir = config.data_dir.clone();
        let backup_dir = config.backup_dir.clone();
        let pledge_book = pledge_book.clone();
        let bandwidth_book = bandwidth_book.clone();
        let cycle_stats = cycle_stats.clone();
        let reconcile_stats = reconcile_stats.clone();
        let relay_url = config.relay_url.clone();
        let passphrase = config.passphrase.clone();
        let stale_after_secs = config.stale_after_secs;
        tokio::spawn(async move {
            if let Err(err) = dashboard::run(
                &config_dashboard_bind,
                node_id_str,
                data_dir,
                backup_dir,
                pledge_book,
                bandwidth_book,
                cycle_stats,
                reconcile_stats,
                endpoint,
                relay_url,
                passphrase,
                stale_after_secs,
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
        let bandwidth_book = bandwidth_book.clone();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let data_dir = data_dir.clone();
                let pledge_book = pledge_book.clone();
                let bandwidth_book = bandwidth_book.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        receive::handle_incoming(incoming, data_dir, pledge_book, bandwidth_book).await
                    {
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
        let bandwidth_book = bandwidth_book.clone();
        let cycle_stats = cycle_stats.clone();
        let reconcile_stats = reconcile_stats.clone();
        async move {
            // When each buddy's manifest was last reconciled against what
            // they actually report holding for us (see
            // backup::reconcile_with_buddy) — separate from the ordinary
            // ~30s cycle above, and deliberately much less frequent, since
            // it costs the buddy a full file listing rather than only
            // touching the network when something local changed. Declared
            // here, outside the loop below, so it persists across ticks
            // for the life of this process.
            let mut last_reconcile: std::collections::HashMap<String, std::time::Instant> =
                std::collections::HashMap::new();
            const RECONCILE_INTERVAL: Duration = Duration::from_secs(600); // 10 minutes
            loop {
                match fetch_buddy_devices(&http, &config, &bandwidth_book).await {
                    Ok((true, _buddies)) => {
                        // Account is paused (relay_status != 'active' —
                        // see stripe-webhook.js) — per product decision,
                        // this stops ALL buddy syncing, not just relay
                        // traffic, so nothing here opens a connection,
                        // sends a file, pings, or reconciles until the
                        // account is active again. The dashboard banner
                        // (dashboard.html) and the payment-failed email
                        // are what the account holder actually sees; this
                        // is just honoring that pause on the wire.
                        tracing::warn!(
                            "this account's relay syncing is paused (billing issue) — \
                             skipping all buddy activity this cycle"
                        );
                    }
                    Ok((false, buddies)) => {
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
                                if let Ok(stats) = &result
                                    && let Some(connection) = &stats.connection
                                {
                                    bandwidth::record(
                                        &config.data_dir,
                                        &bandwidth_book,
                                        connection,
                                        stats.bytes_sent,
                                    )
                                    .await;
                                }
                                let record = match &result {
                                    Ok(stats)
                                        if stats.files_sent == 0
                                            && stats.files_deleted == 0
                                            && stats.files_failed == 0 =>
                                    {
                                        // Truly idle: run_backup_cycle found nothing new to
                                        // send and, per its own doc comment, never opened a
                                        // connection at all — so unlike the general "nothing
                                        // sent" case below, reachability here has not actually
                                        // been checked by anything yet. Piggyback the same
                                        // lightweight Ping the no-BACKUP_DIR branch below already
                                        // uses as its own connectivity check, so a dead buddy
                                        // doesn't sit at "backup ok" forever just because there
                                        // was nothing to resend.
                                        let ping_result =
                                            ping(&endpoint, &config.relay_url, buddy_id).await;
                                        dashboard::CycleRecord {
                                            label: label.clone(),
                                            at: dashboard::unix_now(),
                                            kind: "backup",
                                            ok: ping_result.is_ok(),
                                            detail: match &ping_result {
                                                Ok(()) => {
                                                    "nothing new to back up (connectivity check ok)"
                                                        .to_string()
                                                }
                                                Err(err) => format!(
                                                    "nothing new to back up, but connectivity \
                                                     check failed: {err}"
                                                ),
                                            },
                                            files_sent: Some(0),
                                            files_deleted: Some(0),
                                            bytes_sent: None,
                                            duration_ms: None,
                                            connection: None,
                                            files_failed: Some(0),
                                            failed_files: Vec::new(),
                                            last_ok_at: None,
                                            consecutive_failures: 0,
                                            // This is now backed by a real Ping instead of an
                                            // unconditional assumption — the actual reachability
                                            // signal "stale" is meant to track.
                                            made_progress: ping_result.is_ok(),
                                            last_synced_at: None,
                                        }
                                    }
                                    Ok(stats) if stats.files_sent == 0 && stats.files_deleted == 0 => {
                                        dashboard::CycleRecord {
                                            label: label.clone(),
                                            at: dashboard::unix_now(),
                                            kind: "backup",
                                            ok: false,
                                            detail: format!(
                                                "{} file(s) still failing to sync",
                                                stats.files_failed
                                            ),
                                            files_sent: Some(0),
                                            files_deleted: Some(0),
                                            bytes_sent: None,
                                            duration_ms: None,
                                            connection: None,
                                            files_failed: Some(stats.files_failed),
                                            failed_files: stats
                                                .failed_paths
                                                .iter()
                                                .cloned()
                                                .map(dashboard::FailedFileView::from)
                                                .collect(),
                                            last_ok_at: None,
                                            consecutive_failures: 0,
                                            // files_failed > 0 here means run_backup_cycle *did*
                                            // open a connection and attempt to send (e.g. pledge
                                            // enforcement rejected it) — reachability is already
                                            // proven for this cycle, no extra ping needed. A
                                            // permanently-stuck file (over pledge, too large,
                                            // etc.) fails every single cycle forever by
                                            // definition, so gating progress on files_failed == 0
                                            // would make a perfectly healthy connection look
                                            // "stale — last synced never" permanently. That
                                            // specific-file problem is already called out loudly
                                            // by the failed-files line and the "backup failed"
                                            // pill right next to it — staleness doesn't need to
                                            // repeat it.
                                            made_progress: true,
                                            last_synced_at: None,
                                        }
                                    }
                                    Ok(stats) => dashboard::CycleRecord {
                                        label: label.clone(),
                                        at: dashboard::unix_now(),
                                        kind: "backup",
                                        ok: stats.files_failed == 0,
                                        detail: "backup cycle complete".to_string(),
                                        files_sent: Some(stats.files_sent),
                                        files_deleted: Some(stats.files_deleted),
                                        bytes_sent: Some(stats.bytes_sent),
                                        duration_ms: Some(stats.duration_ms),
                                        connection: stats.connection,
                                        files_failed: Some(stats.files_failed),
                                        failed_files: stats
                                            .failed_paths
                                            .iter()
                                            .cloned()
                                            .map(dashboard::FailedFileView::from)
                                            .collect(),
                                        last_ok_at: None,
                                        consecutive_failures: 0,
                                        // At least one file actually moved this cycle,
                                        // regardless of whether others are still stuck.
                                        made_progress: true,
                                        last_synced_at: None,
                                    },
                                    Err(err) => dashboard::CycleRecord {
                                        label: label.clone(),
                                        at: dashboard::unix_now(),
                                        kind: "backup",
                                        ok: false,
                                        detail: err.to_string(),
                                        files_sent: None,
                                        files_deleted: None,
                                        bytes_sent: None,
                                        duration_ms: None,
                                        connection: None,
                                        files_failed: None,
                                        failed_files: Vec::new(),
                                        last_ok_at: None,
                                        consecutive_failures: 0,
                                        made_progress: false,
                                        last_synced_at: None,
                                    },
                                };
                                // Log against `record` (not `result` directly) so the idle
                                // case's log reflects the ping we just ran instead of
                                // re-deriving a verdict without it.
                                match &result {
                                    Ok(stats) if stats.files_sent == 0 && stats.files_deleted == 0 => {
                                        if record.ok {
                                            tracing::debug!(
                                                buddy = %label,
                                                "nothing new to back up (connectivity check ok)"
                                            )
                                        } else if stats.files_failed == 0 {
                                            tracing::warn!(
                                                buddy = %label,
                                                detail = %record.detail,
                                                "nothing new to back up, and connectivity check failed"
                                            )
                                        } else {
                                            tracing::warn!(
                                                buddy = %label,
                                                files_failed = stats.files_failed,
                                                "backup cycle complete with file(s) still failing to sync"
                                            )
                                        }
                                    }
                                    Ok(stats) => tracing::info!(
                                        buddy = %label,
                                        files_sent = stats.files_sent,
                                        files_deleted = stats.files_deleted,
                                        bytes_sent = stats.bytes_sent,
                                        duration_ms = stats.duration_ms,
                                        "backup cycle complete"
                                    ),
                                    Err(err) => tracing::warn!(buddy = %label, ?err, "backup cycle failed"),
                                }
                                dashboard::record_cycle(&cycle_stats, node_id_str, record);

                                // Periodic reconciliation — independent of
                                // whether this particular cycle had anything
                                // to send, since the whole point is to catch
                                // drift that never shows up as a local file
                                // change in the first place (see
                                // backup::reconcile_with_buddy's doc comment).
                                let reconcile_due = last_reconcile
                                    .get(node_id_str)
                                    .map(|at: &std::time::Instant| at.elapsed() >= RECONCILE_INTERVAL)
                                    .unwrap_or(true);
                                if reconcile_due {
                                    last_reconcile
                                        .insert(node_id_str.clone(), std::time::Instant::now());
                                    let reconcile_result = backup::reconcile_with_buddy(
                                        &endpoint,
                                        &config.relay_url,
                                        &config.data_dir,
                                        buddy_id,
                                    )
                                    .await;
                                    let record = match &reconcile_result {
                                        Ok(stats) => dashboard::ReconcileRecord {
                                            at: dashboard::unix_now(),
                                            checked: stats.checked,
                                            missing_on_buddy: stats.missing_on_buddy.clone(),
                                            unexpected_on_buddy: stats.unexpected_on_buddy.clone(),
                                            error: None,
                                        },
                                        Err(err) => dashboard::ReconcileRecord {
                                            at: dashboard::unix_now(),
                                            checked: 0,
                                            missing_on_buddy: Vec::new(),
                                            unexpected_on_buddy: Vec::new(),
                                            error: Some(err.to_string()),
                                        },
                                    };
                                    reconcile_stats.lock().unwrap().insert(node_id_str.clone(), record);
                                    match reconcile_result {
                                        Ok(stats)
                                            if stats.missing_on_buddy.is_empty()
                                                && stats.unexpected_on_buddy.is_empty() =>
                                        {
                                            tracing::debug!(
                                                buddy = %label,
                                                checked = stats.checked,
                                                "reconciliation: buddy's stored files match our manifest"
                                            );
                                        }
                                        Ok(stats) => tracing::warn!(
                                            buddy = %label,
                                            checked = stats.checked,
                                            missing_on_buddy = stats.missing_on_buddy.len(),
                                            missing_paths = ?stats.missing_on_buddy,
                                            unexpected_on_buddy = stats.unexpected_on_buddy.len(),
                                            unexpected_paths = ?stats.unexpected_on_buddy,
                                            "reconciliation found drift between our manifest and \
                                             the buddy's actual files — missing ones will be \
                                             re-sent automatically next cycle"
                                        ),
                                        Err(err) => tracing::warn!(
                                            buddy = %label, ?err,
                                            "reconciliation check against buddy's actual files failed"
                                        ),
                                    }
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
                                    bytes_sent: None,
                                    duration_ms: None,
                                    connection: None,
                                    files_failed: None,
                                    failed_files: Vec::new(),
                                    last_ok_at: None,
                                    consecutive_failures: 0,
                                    made_progress: result.is_ok(),
                                    last_synced_at: None,
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

// Returns (paused, buddy_devices) — see BuddiesResponse::paused.
async fn fetch_buddy_devices(
    http: &reqwest::Client,
    config: &Config,
    bandwidth_book: &bandwidth::BandwidthBook,
) -> Result<(bool, Vec<BuddyDevice>)> {
    // Piggyback relay-bandwidth reporting on this existing poll (see
    // apps/api/src/routes/devices.js) rather than a separate endpoint —
    // whole megabytes of relay traffic accumulated since the last
    // successful report. Only advance the high-water mark (below) once the
    // request actually succeeds, so a failed poll just re-reports the same
    // bytes next cycle instead of losing them.
    let relay_mb = bandwidth::pending_relay_mb(bandwidth_book);

    let resp = http
        .get(format!(
            "{}/devices/me/buddies?relay_mb={}",
            config.api_url, relay_mb
        ))
        .bearer_auth(&config.device_token)
        .send()
        .await?
        .error_for_status()?;

    if relay_mb > 0 {
        bandwidth::mark_relay_reported(&config.data_dir, bandwidth_book, relay_mb).await;
    }

    let resp: BuddiesResponse = resp.json().await?;
    Ok((resp.paused, resp.buddy_devices))
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
