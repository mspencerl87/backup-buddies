# Backup Buddies client

The agent that actually talks to your buddy's machine over Iroh (P2P,
NAT-traversal via `relay.filegarden.net` when a direct connection isn't
possible) and exchanges backup data. Everything up to this point — the
website, your account, billing, pairing with a buddy — is just
bookkeeping; this is the piece that does the real work.

**Current status:** this first version is a connectivity proof, not full
backup/restore yet. It registers your device, finds your paired buddy's
device through the API, and sends a small end-to-end-encrypted test
payload back and forth to confirm the whole pipe works — your machine,
your buddy's machine, and the relay in between. Real file-backup logic
(watching a folder, chunking, versioning, restore) is the next piece
built on top of this foundation.

## What it does and doesn't do

- **Does:** generate and persist a unique Iroh identity for this device,
  register it with your account, discover your buddy's device, connect to
  it (directly if possible, through the relay otherwise), and encrypt
  everything with `BACKUP_PASSPHRASE` *before* it ever leaves this
  machine.
- **Does not:** send your passphrase anywhere. It's used locally to
  derive an encryption key and never appears in any API call, log line,
  or network request. If you lose it, data stored with your buddy cannot
  be recovered — there's no "forgot passphrase" flow, by design.
- **Does not:** let your buddy read your data. Their client stores
  whatever bytes you send it verbatim, without trying to decrypt them.

## Running it

If you don't already have this directory (e.g. you don't have access to
the project's private source repo), get it with:

```bash
curl -fsSL https://app.filegarden.net/client/install.sh | bash
cd backup-buddies-client
```

Then fill in `.env` — a device token from your dashboard's Devices card,
your own passphrase, and the API URL (you can usually leave that last one
as-is) — and:

```bash
docker compose up -d
docker compose logs -f
```

You should see a line confirming your device registered with the API.
Once your buddy's client is also running (and your pairing is active),
you'll see a "connectivity test succeeded" line roughly every 30 seconds.

## Building locally

```bash
cargo build --release
# or, to build the Docker image yourself instead of pulling it:
docker build -t backup-buddies-client .
```

## Data directory

Everything persistent lives under `/data` inside the container (the
`client_data` named volume in `docker-compose.yml`):

- `/data/iroh_secret_key` — this device's identity. Deleting it means the
  device re-registers under a new identity next startup (update it on
  your dashboard, or just remove and re-add the device there).
- `/data/received/` — encrypted blobs received from a buddy during the
  connectivity test. Not yet organized into real backups — that's part of
  the next phase of this client.
