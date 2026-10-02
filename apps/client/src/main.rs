use std::{
    collections::HashMap,
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
use sha2::{Digest, Sha256};

const ALPN: &[u8] = b"backup-buddies/test/0";
const MAX_TEST_PAYLOAD: usize = 10 * 1024 * 1024; // 10MB cap for this connectivity-proof transfer

struct Config {
    api_url: String,
    device_token: String,
    passphrase: SecretString,
    relay_url: RelayUrl,
    data_dir: PathBuf,
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

        Ok(Self {
            api_url,
            device_token,
            passphrase: SecretString::from(passphrase),
            relay_url,
            data_dir,
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

// How much of a buddy's pledge we've used (received_bytes) against how
// much they've actually pledged us (pledged_bytes). Keyed by the buddy's
// Iroh node id (EndpointId's string form) in the map this is stored in.
#[derive(Clone, Copy, Default)]
struct PledgeState {
    pledged_bytes: u64,
    received_bytes: u64,
}

// Shared between the accept loop (which enforces the pledge on incoming
// data) and the outbound poll loop (which refreshes pledged_bytes from the
// API every 30s — pledges can change if a pairing is re-created). A plain
// std::sync::Mutex is fine here: every critical section is a quick map
// read/write with no .await inside it, never held across one.
type PledgeBook = Arc<Mutex<HashMap<String, PledgeState>>>;

// received_bytes must survive restarts — otherwise a buddy could get a
// fresh budget just by waiting for us to redeploy. Persisted as plain JSON
// at DATA_DIR/usage.json: { "<node_id>": <received_bytes> }. pledged_bytes
// itself isn't persisted — it's re-learned from the API on every poll, so
// there's no stale-pledge risk from a crash mid-write.
async fn load_usage_ledger(data_dir: &Path) -> HashMap<String, u64> {
    match tokio::fs::read(data_dir.join("usage.json")).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

async fn save_usage_ledger(data_dir: &Path, book: &PledgeBook) -> Result<()> {
    let snapshot: HashMap<String, u64> = {
        let book = book.lock().unwrap();
        book.iter().map(|(id, state)| (id.clone(), state.received_bytes)).collect()
    };
    let json = serde_json::to_vec(&snapshot)?;
    tokio::fs::write(data_dir.join("usage.json"), json).await?;
    Ok(())
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
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::custom([config.relay_url.clone()]))
        .bind()
        .await
        .context("failed to bind Iroh endpoint")?;

    tracing::info!(node_id = %endpoint_id, "backup-buddies client starting");

    let http = reqwest::Client::new();

    register_node_id(&http, &config, &endpoint_id).await?;

    // Seed the pledge book with what we've received from each buddy so
    // far (pledged_bytes is left at 0 for now — the poll loop below fills
    // it in on its first run, before accept_task can enforce anything
    // meaningfully; until then, incoming data is rejected, which is the
    // safe default for an unknown pledge).
    let pledge_book: PledgeBook = Arc::new(Mutex::new(
        load_usage_ledger(&config.data_dir)
            .await
            .into_iter()
            .map(|(id, received_bytes)| (id, PledgeState { received_bytes, ..Default::default() }))
            .collect(),
    ));

    // Accept loop: our buddy's client may connect to *us* first. Runs for
    // the life of the process alongside the periodic outbound test below.
    let accept_task = {
        let endpoint = endpoint.clone();
        let data_dir = config.data_dir.clone();
        let pledge_book = pledge_book.clone();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let data_dir = data_dir.clone();
                let pledge_book = pledge_book.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_incoming(incoming, data_dir, pledge_book).await {
                        tracing::warn!(?err, "incoming connection failed");
                    }
                });
            }
        })
    };

    // Periodic loop: ask the control plane who our paired buddies' devices
    // are, and try to prove connectivity to each by sending a small
    // encrypted test payload. This is a connectivity proof, not real
    // backup logic — it proves registration, discovery, NAT traversal via
    // our relay, and client-side encryption all work end to end.
    let outbound_task = {
        let endpoint = endpoint.clone();
        let http = http.clone();
        let pledge_book = pledge_book.clone();
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
                            // (see the accept side's "unknown buddy" check).
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
                            match send_test_payload(&endpoint, &config, buddy_id).await {
                                Ok(()) => tracing::info!(buddy = %label, "connectivity test succeeded"),
                                Err(err) => {
                                    tracing::warn!(buddy = %label, ?err, "connectivity test failed")
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

// Encrypts a small test payload with OUR OWN passphrase (never sent to the
// buddy or the server) and sends it to the buddy's device. The buddy's
// client stores the ciphertext blindly — it has no way to decrypt it. We
// then read back their ack (a hash of what they received) to confirm the
// round trip worked, without the buddy ever seeing plaintext.
async fn send_test_payload(endpoint: &Endpoint, config: &Config, buddy_id: EndpointId) -> Result<()> {
    let plaintext = format!(
        "backup-buddies connectivity test from {} at {}",
        endpoint.id(),
        chrono_now(),
    );

    let recipient = age::scrypt::Recipient::new(config.passphrase.clone());
    let encrypted = age::encrypt(&recipient, plaintext.as_bytes())
        .context("failed to encrypt test payload")?;
    let expected_hash = hex::encode(Sha256::digest(&encrypted));

    let addr = EndpointAddr::new(buddy_id).with_relay_url(config.relay_url.clone());
    let conn = endpoint
        .connect(addr, ALPN)
        .await
        .context("failed to connect to buddy over Iroh")?;

    let (mut send, mut recv) = conn.open_bi().await.context("failed to open stream")?;
    send.write_all(&encrypted).await.context("failed to send test payload")?;
    send.finish().context("failed to finish send stream")?;

    let ack = recv
        .read_to_end(1024)
        .await
        .context("failed to read ack from buddy")?;
    let ack = String::from_utf8_lossy(&ack);

    if ack.trim() != expected_hash {
        anyhow::bail!("buddy's ack did not match — got {ack:?}, expected {expected_hash}");
    }

    Ok(())
}

// Accept-side handler: stores whatever ciphertext arrives, verbatim, under
// a filename keyed by the sender — never attempts to decrypt it, since
// this device has no reason to ever hold the sender's passphrase.
//
// Pledge enforcement lives here: a sender only gets to store up to what
// they've pledged us (buddy_pairings.pledged_bytes, refreshed into
// pledge_book every 30s by the outbound poll loop). An unrecognized
// sender — no pledge on file at all, e.g. the pairing was revoked, or we
// haven't polled since they first appeared — is treated as pledging
// nothing, so they're rejected rather than silently allowed through.
async fn handle_incoming(
    incoming: iroh::endpoint::Incoming,
    data_dir: PathBuf,
    pledge_book: PledgeBook,
) -> Result<()> {
    let connecting = incoming.accept()?;
    let conn = connecting.await?;
    let remote = conn.remote_id();
    let remote_id = remote.to_string();

    let (mut send, mut recv) = conn.accept_bi().await.context("failed to accept stream")?;
    let data = recv
        .read_to_end(MAX_TEST_PAYLOAD)
        .await
        .context("failed to read incoming payload")?;

    // Decide accept/reject before writing anything or updating the
    // ledger — a rejected payload leaves no trace on disk.
    let decision = {
        let mut book = pledge_book.lock().unwrap();
        let state = book.entry(remote_id.clone()).or_default();
        if state.received_bytes.saturating_add(data.len() as u64) > state.pledged_bytes {
            Err((state.pledged_bytes, state.received_bytes))
        } else {
            state.received_bytes += data.len() as u64;
            Ok(())
        }
    };

    let Err((pledged_bytes, received_bytes)) = decision else {
        // Accepted — fall through to the existing write-and-ack path below.
        let hash = hex::encode(Sha256::digest(&data));
        let out_path = data_dir
            .join("received")
            .join(format!("{}-test.age", remote.fmt_short()));
        tokio::fs::write(&out_path, &data).await?;

        if let Err(err) = save_usage_ledger(&data_dir, &pledge_book).await {
            // Don't fail the whole exchange over a ledger write hiccup —
            // worst case we under-persist by one payload's worth and a
            // restart re-allows it, which is the safe direction to err in.
            tracing::warn!(?err, "failed to persist usage ledger");
        }

        send.write_all(hash.as_bytes()).await?;
        send.finish()?;
        send.stopped().await.ok();

        tracing::info!(from = %remote, bytes = data.len(), saved_to = %out_path.display(), "received encrypted test payload");
        return Ok(());
    };

    tracing::warn!(
        from = %remote,
        attempted_bytes = data.len(),
        pledged_bytes,
        received_bytes,
        "rejected incoming payload — would exceed this buddy's pledge"
    );
    // Distinct, readable ack instead of the real hash: the sender's
    // hash-comparison error path (send_test_payload) surfaces this exact
    // string in its own "ack did not match" message, so the rejection
    // reason is visible on their side too, not just ours.
    send.write_all(b"PLEDGE_EXCEEDED").await.ok();
    send.finish().ok();
    send.stopped().await.ok();
    return Ok(());
}

fn chrono_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
