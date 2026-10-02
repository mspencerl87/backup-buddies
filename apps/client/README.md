# Backup Buddies client

The agent that actually talks to your buddy's machine over Iroh (P2P,
NAT-traversal via `relay.filegarden.net` when a direct connection isn't
possible) and exchanges backup data. Everything up to this point — the
website, your account, billing, pairing with a buddy — is just
bookkeeping; this is the piece that does the real work.

**Current status:** real backup and restore. Every 30 seconds it asks the
API who your paired buddies' devices are, then — for any buddy it has a
`BACKUP_DIR` configured for — scans that folder, encrypts anything new or
changed with `BACKUP_PASSPHRASE`, and sends just the diff to that buddy's
device. Deleted files are told apart too, so your buddy's copy mirrors
yours rather than only ever growing. A buddy you're not backing anything
up to still gets a lightweight connectivity ping on the same schedule, so
"is this still working" never depends on actually having something to
send.

## What it does and doesn't do

- **Does:** generate and persist a unique Iroh identity for this device,
  register it with your account, discover your buddy's device, connect to
  it (directly if possible, through the relay otherwise), and encrypt
  everything with `BACKUP_PASSPHRASE` *before* it ever leaves this
  machine.
- **Does:** keep sending only the delta each cycle — unchanged files are
  skipped by comparing their hash against what this client last told a
  given buddy it sent them (see "Data directory" below), not re-sent in
  full every 30 seconds.
- **Does not:** send your passphrase anywhere. It's used locally to
  derive an encryption key and never appears in any API call, log line,
  or network request. If you lose it, data stored with your buddy cannot
  be recovered — there's no "forgot passphrase" flow, by design.
- **Does not:** let your buddy read your data. Their client stores
  whatever bytes you send it verbatim, without trying to decrypt them —
  and only ever lets *you* list or fetch back what *you* stored with
  them, never another buddy's data.

## Running it

If you don't already have this directory (e.g. you don't have access to
the project's private source repo), get it with:

```bash
curl -fsSL https://app.filegarden.net/client/install.sh | bash
cd backup-buddies-client
```

Then fill in `.env` — a device token from your dashboard's Devices card,
your own passphrase, the folder you want backed up (`BACKUP_DIR_HOST`,
optional — leave it unset if this device is only meant to receive), and
the API URL (you can usually leave that last one as-is) — and:

```bash
docker compose up -d
docker compose logs -f
```

You should see a line confirming your device registered with the API.
Once your buddy's client is also running (and your pairing is active),
you'll see either "backup cycle complete" (if you set `BACKUP_DIR_HOST`)
or "connectivity check succeeded" roughly every 30 seconds.

## Restoring

To pull everything a buddy is holding for you back down and decrypt it:

```bash
docker compose run --rm client restore <buddy's node id> /data/restored
```

Find the buddy's node id on your dashboard's Buddies card. Files land
under `restored/` inside your `DATA_DIR_HOST` folder on the host,
decrypted with the `BACKUP_PASSPHRASE` in your `.env` — so this only
works from a machine configured with the *same* passphrase that encrypted
the data in the first place. This is a one-shot operation: the client
restores everything it's offered and exits, rather than running as part
of the normal daemon loop.

## Building locally

```bash
cargo build --release
# or, to build the Docker image yourself instead of pulling it:
docker build -t backup-buddies-client .
```

## Data directory

Everything persistent lives under `/data` inside the container, bind-mounted
from `DATA_DIR_HOST` on the host (see `.env.example` — defaults to `./data`):

- `/data/iroh_secret_key` — this device's identity. Deleting it means the
  device re-registers under a new identity next startup (update it on
  your dashboard, or just remove and re-add the device there).
- `/data/received/<buddy node id>/` — the actual files a buddy has backed
  up with you, stored encrypted and under the same relative paths they
  back them up with on their end. This device never attempts to decrypt
  them — it has no reason to ever hold the sender's passphrase.
- `/data/manifest-<buddy node id>.json` — what *this* device last told
  that buddy it had sent them (path, size, hash per file). Compared
  against `BACKUP_DIR` every cycle to figure out what's changed. Deleting
  it just means the next cycle re-sends everything — wasteful, but
  harmless; it doesn't affect what's actually stored on either side.
- `/data/usage.json` — `{ "<buddy node id>": <bytes received from them> }`,
  tracked per buddy and checked against their pledge
  (`buddy_pairings.pledged_bytes`, fetched from the API every 30s) before
  accepting any incoming file. Deleting it resets what's counted toward
  each buddy's pledge back to zero — the underlying data in `received/` is
  untouched, so this can desync the two if used carelessly.
- `/data/restored/` — default destination for `restore` (see above); only
  appears after you run it.
