# Backup Buddies: notes for Claude

Peer-to-peer encrypted backup: each user's client backs up to a "buddy's"
device over iroh (direct, or through our relay). Live at filegarden.net
(app/API) and relay.filegarden.net. Owner: Spencer.

## Layout

- `apps/client`: Rust backup client (Docker image, also buildable from
  source via `install.sh`). Backs up into an encrypted restic-format
  repository on the buddy (rustic_core, `chunked.rs`); key derived in
  `crypto.rs`.
- `apps/api`: Node/Express API (Postgres). Device-facing routes in
  `routes/devices.js` (bearer device token), browser routes use sessions.
- `apps/web`: static site + dashboard + admin (plain HTML/JS).
- `db/migrations`: numbered SQL, applied **by hand** on app-1 (see
  `docs/DEPLOY.md`); nothing runs them automatically.
- `deploy/app`, `deploy/relay`: compose files for app-1 and relay-1.
- `docs/`: `CHECKLIST.md` (the one list of open work: bugs, tests to
  run, known issues, backlog), `TESTING-CHECKLIST.md` (test history and
  design notes only), `DEPLOY.md` (numbered sections
  per feature: add one when a change needs deploy steps), `API.md` (every
  route) and `OPERATIONS.md` (settings, jobs, scripts, CI, commands).
  Update those two, and the client README's "Commands and settings", when
  a route, env var, job, script or client setting changes.

## Rules

- **This whole repo is public** (github.com/mspencerl87/backup-buddies,
  history included). Never commit secrets, real `.env` values, keys or
  tokens anywhere; `.env.example` files hold placeholders only. Licenses:
  `apps/client` is GPL-3.0-only (its own `LICENSE`), everything else
  AGPL-3.0-only (root `LICENSE`), copyright Spencer LeBlanc.
- **`apps/client` is also mirrored on its own** to
  github.com/mspencerl87/backup-buddies-client on every push
  (`.github/workflows/sync-public-client.yml`), so keep it self-contained:
  no references to `docs/`, `apps/api` or deploy notes in anything under
  it. New client source files get the two-line SPDX/copyright header.
  Changes made directly on the mirror are overwritten by the next sync;
  bring them in here.

- **Never run `cargo fmt`.** The client isn't rustfmt-formatted; it would
  rewrite every file. Match the surrounding style by hand.
- **Bump `apps/client/Cargo.toml` version (and the matching entry in
  `Cargo.lock`) on every client change users should get.** Devices compare
  their version against it to show "Update available".
- **`git pull --rebase` before every push.** Several sessions push to
  `main`, and a bot commits "Stamp asset versions" after CSS/JS changes.
- Commit straight to `main` (no PRs); ask Spencer before pushing.
- Comments explain *why* (often with what was seen live). Keep that density.

## What a push to `main` does

- `apps/client/**` → `build-client.yml` builds and publishes
  `ghcr.io/mspencerl87/backup-buddies/client:latest` (~7 min). Users pull it
  themselves, so **build and test before pushing**.
- `apps/api/**`, `apps/web/**`, `deploy/app/**`, `db/migrations/**` →
  `deploy.yml` stamps asset versions, builds the API, and on app-1 runs
  `git pull` + `docker compose up -d` + `caddy reload` (~1.5 min).
- Devices read the latest version from app-1's checkout. `build-client.yml`
  runs `git pull` there itself once the image is published, so a version
  bump shows as "Update available" about 7 minutes after the push.
- Ship API changes the client depends on before (or with) the client.

## Building the client (this Windows PC)

Rust is installed in WSL Ubuntu, not on Windows. Build in WSL with the
target dir on the Linux side (much faster than /mnt/c):

```bash
wsl -e bash -lc 'cd /mnt/c/Users/sleblanc/Desktop/backup-buddies/apps/client && export CARGO_TARGET_DIR=~/bb-target && cargo test --locked && cargo clippy --locked --all-targets'
```

Clippy runs clean (since client 0.20.0); keep it that way. Run git from
Windows, not WSL (line endings make WSL's git see every file as changed).

## Checking API and web changes (this Windows PC)

Node 20 (same as the API image) is in WSL at `~/.local/node`. A syntax
error in any API route stops the whole API from starting, so before pushing:

```bash
wsl -e bash -lc 'rm -rf ~/bb-api && cp -r /mnt/c/Users/sleblanc/Desktop/backup-buddies/apps/api ~/bb-api && cd ~/bb-api && npm ci --silent && for f in $(find src -name "*.js"); do node --check $f; done'
```

For new SQL, run it against PGlite (in-process Postgres: `npm i --no-save
@electric-sql/pglite` in `~/bb-api`) with `db/init.sql` (minus its
`CREATE EXTENSION` line) and every migration applied, by pointing
`pool.query` at it and calling the route handler. For page scripts, pull
the `<script>` out and `node --check` it; the client dashboard's JS lives
in a Rust string in `dashboard.rs`, so the compiler doesn't check it either.

## Things that are easy to get wrong

- The backup's key: scrypt(passphrase, `"backup-buddies/file-key/v2/" +
  account_id`), then the repository's master key is a labelled hash of
  that (`crypto.rs`, `REPO_KEY_LABEL`). Changing the KDF, the salt or the
  label orphans every backup.
- What a buddy can see: only encrypted pack files, how many, their sizes
  and when they change (`chunked.rs`; since client 0.21.0 the only kind of
  backup). Don't claim more than that in site copy.
- rustic_core is pinned exactly (0.x): its own restore isn't used
  (unbounded memory, panics on a failed read), and its backend calls are
  bridged onto iroh in `repo_backend.rs`.
- Test devices: testa / testb on Spencer's `backupbuddy` box (TrueNAS SMB
  and NFS mounts), dashboards on :8080 / :8081.
