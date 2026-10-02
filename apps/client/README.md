# Backup Buddies client

The agent that actually talks to your buddy's machine over Iroh (P2P,
NAT-traversal via `relay.filegarden.net` when a direct connection isn't
possible) and exchanges backup data. Everything up to this point — the
website, your account, billing, pairing with a buddy — is just
bookkeeping; this is the piece that does the real work.

**Current status:** real backup and restore. Every 30 seconds it asks the
API who your paired buddies' devices are, then — if this device has
`BACKUP_DIR` set (which `docker-compose.yml` always does; it just points
at an empty, read-only folder when you haven't set `BACKUP_DIR_HOST`) —
scans that folder, encrypts anything new or changed with
`BACKUP_PASSPHRASE`, and sends just the diff to each buddy's device.
Deleted files are told apart too, so your buddy's copy mirrors yours
rather than only ever growing. A receive-only device (no `BACKUP_DIR_HOST`
set) still runs this same cycle against its empty folder every 30
seconds — you'll see "nothing new to back up" instead of "backup cycle
complete" every time, not a separate connectivity-only message, so "is
this still working" never depends on actually having something to send.

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
- **Does:** keep a short backstop when a file is overwritten or deleted —
  the live copy plus its last 2 replaced versions are kept on your
  buddy's disk, browsable and individually restorable from the local
  dashboard (see "File versions" below). Not full version history, just
  protection against a bad overwrite or an accidental delete.
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
you'll see a cycle line roughly every 30 seconds: "backup cycle complete"
if there was something to send, or "nothing new to back up" if not
(which is what you'll see every time if you haven't set
`BACKUP_DIR_HOST`) — either way, it confirms the connection is working.

Before any of that, the client checks its own setup and refuses to start
with a clear error rather than failing confusingly later — a `BACKUP_DIR`
that doesn't exist, a `DATA_DIR` it can't actually write to, or an
`API_URL` that isn't a valid URL all stop it immediately. A short
`BACKUP_PASSPHRASE` (under 12 characters) doesn't stop it, but logs a
warning — there's no way to recover your data if it's ever lost or
guessed, so a short one is worth reconsidering.

## Status dashboard

This device also serves its own small status page — not to be confused
with your account dashboard on the website — at
**http://&lt;this machine's address&gt;:8080** (or whatever `DASHBOARD_PORT`
you set), once `docker compose up -d` is running. It shows, per buddy:
their pledge to you and how much of it is used, how many files you've
sent each other and how much that totals to, and the result of the last
backup cycle (or ping, for a buddy you're not backing anything up to).
There's also a "Restore from this buddy" button, which does the same
thing as the `restore` command below but writes to
`data/restored/<their node id>/` and shows you the result right there
instead of needing a terminal.

**It has no login of its own**, and that restore button is a real action
— not just a read-only view. By default it's open on every interface
(most people run this headless and manage it from elsewhere on their
network), so anyone who can reach this machine on that port can see your
buddy/pledge info and trigger a restore. If this machine is reachable
from somewhere you don't trust — the open internet, a shared or guest
network — either restrict it to loopback (uncomment the `127.0.0.1:`
version of the port mapping in `docker-compose.yml`) and reach it over
SSH tunnel or VPN instead, or put a reverse proxy with auth in front of
it.

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
- `/data/received/<buddy node id>/.versions/` — backup slots for that
  buddy's files, kept when a file is overwritten or deleted (see "File
  versions" below). Deleting this folder just gives up that recovery
  history; it doesn't touch the live files in `received/`.
- `/data/bandwidth.json` — lifetime bytes moved via relay vs. direct (see
  "Relay bandwidth" below). Deleting it just resets the dashboard's
  counter back to zero.

## Disk safety: pledges vs. the real disk

A pledge is just a number two accounts agreed on over the API — it's not a
reservation the filesystem enforces. On top of the per-buddy pledge check
above, every incoming file is also checked against the *real* free space on
the filesystem under `DATA_DIR` (`statvfs`, same mechanism the dashboard's
Disk card reads). A write that would eat into a floor of whichever is
bigger — 2GB flat, or 5% of the filesystem's total size — is rejected with
"not enough disk space on this host", independent of whether the sender is
still within their pledge. This is what keeps an over-pledge (or anything
else filling the same disk) from being able to actually starve the host,
with no admin intervention needed.

The dashboard also lets you check a buddy's *real* numbers directly — each
buddy card fetches their live disk stats (total/free, and what they've
pledged out to everyone, not just you) over the sync protocol and shows it
next to their pledge, with a "Recheck" button for an on-demand refresh. If
a buddy's actual free space is less than what they've pledged out, it's
called out in red — useful for spotting a buddy who over-pledged before
anything actually fails.

## Network/transfer visibility

Each backup cycle and restore reads its own transfer numbers straight off
the connection: bytes actually sent/received (including protocol overhead,
not just file sizes — a more honest number) and wall-clock duration, shown
on the dashboard as a rate (e.g. "1.2 MB in 3.1s (390 KB/s)") next to that
buddy's last cycle and any restore result.

Also shown: whether the connection to a buddy ended up **direct**
(hole-punched peer-to-peer) or went **via relay**
(`relay.filegarden.net`), plus round-trip latency. iroh always tries direct
first and only falls back to the relay when NAT traversal doesn't work out
— a buddy stuck on relay will generally see lower throughput than a direct
one, so this is usually the first thing to check if someone's transfers
seem slow. All of this is local telemetry read off iroh's own connection
object (`Connection::paths()`/`Connection::stats()`) — none of it is sent
over the wire or costs an extra round trip.

A direct connection is only *possible* if something outside your network
can actually reach this device on its sync port — `SYNC_PORT` in
`.env` (11235 by default), published by `docker-compose.yml`. Forwarding
that same port on your router gives NAT traversal a real shot; without
it, every connection relays no matter how good either side's network
otherwise is, since nothing can ever reach this device from outside to
hole-punch with. Forwarding it is entirely optional — nothing breaks if
you skip it, transfers just relay instead, same as before this existed.
Note that two buddies behind the *same* NAT/public IP (e.g. both on the
same home network as this host) are a case port forwarding can't help —
most routers won't let a device reach another device behind the same NAT
via the NAT's own public address ("hairpinning"), regardless of what's
forwarded.

For buddies who genuinely are on the same local network — two NAS boxes
in the same house, say — the client also runs local discovery over mDNS
(same idea as Syncthing's LAN discovery): each client broadcasts/listens
on the LAN and, if it finds its buddy there, connects over the private
network directly. This sidesteps the relay and the router's NAT/hairpin
behavior entirely rather than trying to traverse it. Look for
`"found a buddy on the local network (mDNS)"` in the logs to confirm
it's working.

This needs `network_mode: host` in `docker-compose.yml` (already set by
default) — mDNS relies on real LAN multicast, which a normally-networked
(bridge-mode) container can't receive no matter what ports are
published, even to another container on the *same host*. Host networking
means the app's `DASHBOARD_PORT`/`SYNC_PORT` now bind directly on the
host rather than through a Docker port mapping, so **each device sharing
a host needs its own distinct values for both** — no longer just "if the
default's taken," but required. It's Linux-only; on Docker Desktop
(Mac/Windows), drop `network_mode: host` and publish ports the normal
way instead — everything still works, just without same-LAN direct
connections (relay and direct-over-internet are unaffected).

There's no live, mid-transfer rate (e.g. "currently uploading at 2MB/s") —
cycles run every 30s and are usually short, so a last-cycle average is
what's shown rather than adding a streaming/websocket layer for numbers
that would mostly read "idle" between cycles anyway.

Lifetime relay-vs-direct totals are also shown, in their own card on the
dashboard — see "Relay bandwidth" below, including how that figure now
actually is wired up to billing.

## File versions — protection against overwrites and accidental deletes

A buddy's copy used to just mirror yours exactly: whatever you last sent
them is all they have. Now, whenever a file you'd already backed up gets
overwritten or deleted, the previous content isn't discarded — it's kept
as a numbered backup slot (`.versions/` inside that buddy's `received/`
folder on the receiving end), up to the 2 most recent replacements. So at
any time, up to 3 copies of a file exist on your buddy's disk: the current
one (if it still exists) plus its last 2 predecessors. A delete isn't
destructive either — the deleted content becomes the newest backup slot
rather than vanishing, so a fat-fingered local delete is recoverable the
same way a bad overwrite is.

To get one back: open **Browse files** on a buddy's card, click
**History** next to a file, and pick a version to restore. It's written
to `data/restored/<buddy>/<path>.v<N>.bak` — alongside, never over, a
normal restore of that file — so you can compare before deciding what to
keep.

This is a backstop against corruption, a bad sync, or an accidental local
delete, not full version history — only the live copy and its last 2
predecessors are kept, and they still count toward your buddy's disk and
(once a path has cycled through all its backup slots) their pledge to you,
same as any other data you store with them.

## Buddy health — staleness and per-file failures

The pill next to each buddy's name shows the *last* cycle's result, but a
buddy could look fine on that alone while quietly failing for a while
(say, a one-off error that happens to land on the very last cycle you
happened to check). The dashboard tracks this separately: if a buddy
hasn't had a *successful* cycle in over `STALE_AFTER_SECS` (10 minutes by
default — see `.env.example`), a red **stale** badge shows up next to
their status, saying how long it's actually been since something last
went through.

Separately, if specific files keep failing to send or delete while the
cycle otherwise runs fine, they're called out by name under that buddy's
card (capped at the first 10, if there are more) rather than just logged
and silently retried forever in the background. A cycle with any failing
files is no longer reported as fully "ok" either, for the same reason.

## Relay bandwidth

Every file transferred — your own backups and restores, and anything a
buddy pushes to or pulls from you — is tallied by which network path
carried it, direct or via `relay.filegarden.net`, and kept as a running
lifetime total in a **Bandwidth** card on the dashboard (persisted to
`data/bandwidth.json`, so it survives restarts). Direct peer-to-peer
transfers don't touch the relay at all and are never billed; only the
relay figure is.

If your account has an active paid subscription, relay bandwidth is billed
separately from storage, at a rate shown on your dashboard's billing card
(whole-megabyte granularity — fractions of a megabyte just carry over to
the next report). Every ~30 seconds, alongside the existing buddy-list
poll, this client reports however many megabytes of relay traffic have
accumulated since its last successful report, and only then advances its
own internal "already reported" marker — so a failed poll re-reports the
same bytes next cycle instead of losing them, and nothing is ever reported
twice. Free-tier accounts are never billed for relay bandwidth, no matter
how much of it they use.
