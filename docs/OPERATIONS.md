# Backup Buddies operations reference

How the service runs: the servers and outside services it depends on,
every setting, the background jobs, the scripts, CI, and the commands for
routine work. For the HTTP routes see [API.md](API.md). For how each
feature was set up (and why) see [DEPLOY.md](DEPLOY.md), which has
numbered sections that are referenced below as "DEPLOY §N".

## Contents

- [How it fits together](#how-it-fits-together)
- [Servers](#servers)
- [Outside services and accounts](#outside-services-and-accounts)
- [Settings: app-1 `.env`](#settings-app-1-env)
- [Settings: relay-1 `.env`](#settings-relay-1-env)
- [Background jobs](#background-jobs)
- [Database](#database)
- [Scripts](#scripts)
- [CI (GitHub Actions)](#ci-github-actions)
- [Routine commands](#routine-commands)
- [Built-in limits and constants](#built-in-limits-and-constants)

## How it fits together

```
 browser ──HTTPS──▶ Cloudflare ──▶ app-1: Caddy ─┬─ /api/*   ─▶ api (Node, :3000) ─▶ postgres
                                                 ├─ /client/* ─▶ apps/client files (install.sh, source)
                                                 └─ else      ─▶ apps/web (static site)
 backup client ─HTTPS─▶ api.filegarden.net ─▶ the same api, unprefixed
 backup client ◀─Iroh/QUIC─▶ buddy's client   (direct when possible)
 backup client ◀─────▶ relay-1 (iroh-relay) ◀─────▶ buddy's client   (when direct fails)
 relay-1 ──POST /relay/access──▶ api   (is this device allowed?)
 Stripe ──webhook──▶ api /webhooks/stripe
```

- **The API never sees file contents or the encryption passphrase.**
  Clients encrypt with age before sending; the key is derived from the
  user's passphrase and their account id (client `crypto.rs`). The API
  holds accounts, devices, pairings, billing state and relay usage.
- **Buddies can't see contents, or (since client 0.14.0) file and folder
  names.** They still see sizes, folder depth, entries per folder, file
  counts and when things change. Files from older clients stay under
  plaintext names until the sender updates and re-sends them.
- **Losing the database is serious:** since client 0.7.0 the account id is
  part of every key, so users couldn't restore on a new machine without
  it. That's why the database has a nightly off-site backup.

## Servers

Both are VPSes running Ubuntu 24.04 with Docker. The repo is checked out
at `$CHECKOUT` on each.

| Host | Plan | Runs | Compose file | Firewall (inbound) |
|---|---|---|---|---|
| **app-1** | 2GB, provider disk backups on | `api` (image from GHCR), `postgres` (16-alpine, volume `pgdata`, not exposed), `caddy` (TLS, static files) | `deploy/app/docker-compose.yml` | 22, 80, 443 |
| **relay-1** | 1GB | `relay` (`n0computer/iroh-relay:v1.3.0`) | `deploy/relay/docker-compose.yml` | 22, 443, 7824/udp (80 for certbot renewals) |

DNS is on Cloudflare (DEPLOY §3):

| Name | Points to | Proxy |
|---|---|---|
| `app.filegarden.net` | app-1 | proxied (orange) |
| `api.filegarden.net` | app-1 | proxied (orange) |
| `relay.filegarden.net` | relay-1 | **DNS only (grey)**: proxying breaks NAT hole-punching |

TLS: Caddy gets and renews app-1's certificates itself. relay-1 uses
certbot (`/etc/letsencrypt/live/relay.filegarden.net/`), with a renewal
hook that restarts the relay container (DEPLOY §6).

The relay's settings are in `deploy/relay/config.toml`: it asks
`https://api.filegarden.net/relay/access` before admitting a device, and
limits each client to 4 MiB/s (16 MiB burst) and new connections to 50/s
(burst 200).

## Outside services and accounts

Everything someone taking over would need access to:

| Service | Used for | Where it's configured |
|---|---|---|
| GitHub `mspencerl87/backup-buddies` (private) | Source, CI, GHCR images (`api`, `client`) | Actions secrets (see [CI](#ci-github-actions)) |
| GitHub `mspencerl87/backup-buddies-client` (public) | GPL mirror of `apps/client` | Deploy key, secret `PUBLIC_CLIENT_DEPLOY_KEY` (DEPLOY §27) |
| VPS provider | app-1, relay-1, firewalls, disk backups | Provider dashboard |
| Cloudflare | DNS and proxy for `filegarden.net` | Cloudflare dashboard |
| Stripe | Subscriptions, Checkout, Billing Portal, relay usage meter, webhooks | `STRIPE_*` env vars; prices made by `scripts/` (DEPLOY §10, §13) |
| Google Cloud | "Continue with Google" OAuth client | `GOOGLE_*` env vars (DEPLOY §9) |
| Purelymail | SMTP for every email (verification, resets, notices, support) and the `support@` mailbox | `SMTP_*` env vars (DEPLOY §14) |
| Backblaze B2 | Nightly database dumps | `B2_*` env vars (DEPLOY §26) |
| Uptime Kuma (optional) | Alert if a nightly backup doesn't happen | `BACKUP_PING_URL` |

## Settings: app-1 `.env`

`$CHECKOUT/deploy/app/.env`, read by the `api` container
(`env_file`) and by compose itself. The template, with comments, is
`.env.example` at the repo root. **The API reads these once at startup:
after a change, run `docker compose up -d` (from `deploy/app`) to recreate
the container.**

| Variable | Required | Default | What it does |
|---|---|---|---|
| `NODE_ENV` | no | | `production` on app-1 |
| `PORT` | no | `3000` | API listen port inside the container |
| `API_IMAGE` | on app-1 | `backup-buddies-api:local` | Image compose runs: `ghcr.io/mspencerl87/backup-buddies/api:latest` |
| `POSTGRES_PASSWORD` | yes | | Postgres password (compose sets up the database with it) |
| `DATABASE_URL` | yes | | `postgresql://backup_buddies:<same password>@postgres:5432/backup_buddies` |
| `APP_URL` | yes | | `https://app.filegarden.net`, no trailing slash. Used for links in emails and Stripe redirects; checkout returns `503` without it |
| `ADMIN_EMAILS` | for admin | empty | Comma-separated emails allowed into `/admin.html` and `/admin/*`. Also who gets the stale-account digest. Empty means nobody is admin |
| `STRIPE_SECRET_KEY` | for billing | | `sk_test_…` or `sk_live_…` |
| `STRIPE_WEBHOOK_SECRET` | for billing | | `whsec_…` for `/webhooks/stripe` |
| `STRIPE_STORAGE_PRICE_ID` | for billing | | The flat $3/month plan price (`scripts/create-stripe-storage-price.mjs`). The name is from when it was a per-TB storage price |
| `STRIPE_RELAY_PRICE_ID` | no | | The metered relay price. Setting it adds relay to new checkouts and turns on the 15-minute usage sync to Stripe |
| `STRIPE_RELAY_METER_ID` | no | | For reference only; no code reads it |
| `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `GOOGLE_REDIRECT_URI` | no | | Google sign-in. If any is missing, the Google button just reports "not configured". Redirect URI: `https://app.filegarden.net/api/auth/google/callback` |
| `SMTP_HOST`, `SMTP_USER`, `SMTP_PASS` | for email | | If any is missing, emails are logged instead of sent |
| `SMTP_PORT` | no | `465` | 465 means implicit TLS; anything else uses STARTTLS |
| `MAIL_FROM` | no | `SMTP_USER` | From address, e.g. `"Backup Buddies <you@yourdomain.com>"` |
| `SUPPORT_EMAIL` | no | `support@filegarden.net` | Where the support form goes, and the reply-to on notices |
| `RELAY_ACCESS_TOKEN` | yes | | Shared secret with relay-1 (`openssl rand -hex 32`). If unset, every relay connection is refused |
| `RELAY_HOSTNAME` | no | | For reference; no code reads it |
| `FREE_RELAY_GB_PER_MONTH` | no | `10` | Monthly relay allowance in GB, on both plans: the cap for free accounts, included before billing for paid ones |
| `STALE_ACCOUNT_CLEANUP_ENABLED` | no | off | `true` turns on the inactivity warning emails and the monthly admin digest |
| `STALE_CLEANUP_EXEMPT_EMAILS` | no | | Comma-separated emails never flagged as stale (admins never are) |
| `B2_KEY_ID`, `B2_APP_KEY`, `B2_BUCKET` | for backups | | Read by `backup-db.sh`, not the API. Use a key limited to that bucket that can't delete |
| `BACKUP_PING_URL` | no | | Uptime Kuma push URL, called after each successful backup |

## Settings: relay-1 `.env`

`$CHECKOUT/deploy/relay/.env` (template:
`deploy/relay/.env.example`).

| Variable | Required | What it does |
|---|---|---|
| `RELAY_ACCESS_TOKEN` | yes | Same value as on app-1. Passed to iroh-relay as `IROH_RELAY_HTTP_BEARER_TOKEN`; compose won't start without it |

## Background jobs

They run inside the API process on timers (`src/index.js`). There's no
cron and no separate worker. Each is safe to run twice, so restarts don't
cause problems.

| Job | File | When | Does | Enabled by |
|---|---|---|---|---|
| Relay usage sync | `jobs/sync-relay-usage.js` | Every 15 min, plus 1 min after start | Adds up unsynced `relay_usage_events` per account with a Stripe customer, reports whole MB to the Stripe meter `relay_bandwidth_mb`, marks them synced. Leftover fractions wait for the next run | `STRIPE_RELAY_PRICE_ID` set |
| Free relay cap reset | `jobs/reset-free-relay-cap.js` | Hourly, plus 2 min after start | Resumes accounts paused with `free_relay_cap` before the current month (UTC) began | always |
| Stale-account warning | `jobs/stale-account-cleanup.js` | Daily, plus 5 min after start | Emails accounts with no device activity in 30 days (or since signup if they have no devices); clears the warning when activity comes back | `STALE_ACCOUNT_CLEANUP_ENABLED=true` |
| Stale-account digest | `jobs/stale-account-digest.js` | Checked daily, sent every 30 days (last send kept in `job_runs`) | Emails `ADMIN_EMAILS` a list of accounts still inactive 30 days after their warning. **It never deletes anything**: deleting is done by hand from `/admin.html` | same |

Things that happen on request, not on a timer:
- **Free relay cap:** checked whenever a free account's client reports
  relay usage. Crossing it pauses the account (reason `free_relay_cap`)
  and emails the user.
- **Failed payment:** paused by the Stripe webhook once Stripe stops
  retrying.

Outside the API: **nightly database backup**, `deploy/app/backup-db.sh`,
run by the systemd timer `bb-db-backup.timer` at 03:30 UTC (DEPLOY §26).

## Database

PostgreSQL 16 in the `postgres` container on app-1, database and user
both `backup_buddies`. Only the API container can reach it.

**Schema:** `db/init.sql` (runs automatically only when the `pgdata`
volume is first created) plus `db/migrations/002…016`, **applied by hand,
in order**. Nothing applies migrations automatically. A brand-new database
therefore needs init.sql (automatic) followed by every migration. A
migration that changes columns the code reads must be applied *before*
the code that uses it is pushed; DEPLOY.md says so for each one.

| Table | Holds |
|---|---|
| `accounts` | Users: email, password hash / Google id, verification and reset tokens (hashes), Stripe ids, `relay_status` / paused-at / reason, `billing_exempt`, nickname, stale-warning date |
| `sessions` | Browser sessions (id = cookie value, 30-day expiry) |
| `devices` | Clients: account, label, token hash and last four, Iroh node id, last seen, client version |
| `invites` | Offers: code, inviter, pledged bytes, status, public listing fields, who redeemed it and when |
| `buddy_pairings` | Active and revoked pairings between two accounts. One active pairing per pair of accounts (unique index, migration 011) |
| `relay_usage_events` | Relay bytes per report, and whether they've been synced to Stripe |
| `job_runs` | Last run time of monthly jobs |

## Scripts

All are run by hand from the repo root with Node 20+, except the backup
script. None needs `npm install`.

| Command | Does |
|---|---|
| `STRIPE_SECRET_KEY=sk_… node scripts/create-stripe-storage-price.mjs` | Creates the plan product and flat $3/month price and prints the id for `STRIPE_STORAGE_PRICE_ID`. Each run makes a **new** price |
| `STRIPE_SECRET_KEY=sk_… node scripts/create-stripe-relay-meter-and-price.mjs` | Creates the relay billing meter (event `relay_bandwidth_mb`) and the $0.02/GB metered price, and prints ids for `STRIPE_RELAY_PRICE_ID` / `STRIPE_RELAY_METER_ID`. Each run makes **new** ones |
| `STRIPE_SECRET_KEY=… STRIPE_STORAGE_PRICE_ID=… STRIPE_RELAY_PRICE_ID=… node scripts/add-relay-price-to-existing-subscriptions.mjs` | Adds the relay price to every active subscription that has storage but not relay (subscriptions from before relay billing). Safe to re-run |
| `STRIPE_SECRET_KEY=… OLD_STORAGE_PRICE_ID=… STRIPE_STORAGE_PRICE_ID=… node scripts/move-subscriptions-to-flat-plan.mjs [--apply]` | Lists every live subscription still on the old per-TB price with what it pays now vs. the flat price (flagging who goes up); **`--apply`** swaps each one's item to the flat price, quantity 1, from the next renewal. Dry run without it. Safe to re-run |
| `node scripts/stamp-asset-versions.mjs` | Rewrites `?v=<hash>` on CSS/JS references in `apps/web/*.html` so browsers fetch changed files. CI runs it on every deploy |
| `node scripts/stamp-asset-versions.mjs --check` | **`--check`**: changes nothing; exits 1 if any reference is out of date |
| `deploy/app/backup-db.sh` | `pg_dump` (gzipped) uploaded to B2, then pings `BACKUP_PING_URL`. Normally run by the timer; to run one now: `sudo systemctl start bb-db-backup.service` |

## CI (GitHub Actions)

Every workflow runs on a push to `main` that touches its paths.

| Workflow | Paths | Does | Takes |
|---|---|---|---|
| `deploy.yml` | `apps/api/**`, `apps/web/**`, `deploy/app/**`, `db/migrations/**` | 1. Stamps asset versions and commits "Stamp asset versions" if anything changed. 2. Builds and pushes `ghcr.io/mspencerl87/backup-buddies/api:latest` (and `:<sha>`). 3. Over SSH on app-1: `git pull`, `docker compose pull api`, `docker compose up -d`, prune images. **Doesn't run migrations** | ~1.5 min |
| `build-client.yml` | `apps/client/**` | Builds and publishes `ghcr.io/mspencerl87/backup-buddies/client:latest` (and `:<sha>`), then over SSH on app-1 runs `git pull --ff-only`, so `/client/Cargo.toml` (what devices and the web dashboard compare against) only moves once the image exists | ~7 min |
| `sync-public-client.yml` | `apps/client/**` (can also be run by hand) | Copies `apps/client` to the public repo as one commit | seconds |

app-1's checkout is where devices read the latest client version from.
`build-client.yml` pulls it once the image is published (its
`publish-version` job; it shares the `app-1-checkout` concurrency group
with `deploy.yml`'s deploy job so the two never `git pull` at once). If
that job fails, run `git pull` in `$CHECKOUT` on app-1 by hand.

Repository secrets: `LINODE_APP_HOST`, `LINODE_APP_USER`,
`LINODE_APP_SSH_KEY` (deploy over SSH), `PUBLIC_CLIENT_DEPLOY_KEY`
(public mirror). `GITHUB_TOKEN` is provided automatically.

## Routine commands

On **app-1**, from `$CHECKOUT/deploy/app` unless noted.

```bash
# Is it up?
curl -s https://api.filegarden.net/healthz
curl -s https://api.filegarden.net/readyz
docker compose ps

# API logs (live; Ctrl+C to stop). Logs reset whenever the container is recreated.
docker compose logs -f --tail 100 api
# Only admin actions
docker compose logs api | grep '\[admin\]'
# Rate-limit hits, support requests
docker compose logs api | grep -E '\[rate-limit\]|\[support\]'

# Restart / pick up .env changes
docker compose up -d
docker compose restart api

# Deploy by hand (what CI does)
cd $CHECKOUT && git pull && cd deploy/app && docker compose pull api && docker compose up -d

# SQL shell
docker compose exec postgres psql -U backup_buddies -d backup_buddies

# Apply a migration (file must already be on app-1)
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies < ../../db/migrations/NNN_name.sql

# Or one statement, before the file has been pushed
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies -c "…;"

# Backups
systemctl list-timers bb-db-backup.timer
sudo systemctl start bb-db-backup.service
journalctl -u bb-db-backup.service -n 50
```

Restoring a database dump is in DEPLOY §26.

On **relay-1**, from `$CHECKOUT/deploy/relay`:

```bash
docker compose logs -f --tail 100 relay
docker compose restart relay
cd $CHECKOUT && git pull && cd deploy/relay && docker compose up -d   # update (no CI for relay-1)
```

## Built-in limits and constants

Changing these means a code change, not a setting.

| What | Value | Where |
|---|---|---|
| Free pledge allowance | 50 GB | `routes/billing.js` `FREE_GB` |
| Paid plan | $3/month flat, any pledge size | Stripe price; `routes/billing.js` `PLAN_USD_PER_MONTH` |
| Relay price | $0.02/GB past the monthly allowance (paid accounts) | `lib/relay-billing.js`, Stripe price |
| Pledge sizes | 5 GB steps, 5 GB to 20,000 GB | `routes/invites.js`; also `apps/web/pledge-options.js` |
| Largest relay report accepted | 100,000 MB per poll | `lib/relay-billing.js` |
| Session length | 30 days | `routes/auth.js` |
| Verification link | 48 h, single use, 60 s resend cooldown | `lib/email-verification.js` |
| Password reset link | 24 h, single use, 60 s cooldown | `lib/password-reset.js` |
| Minimum password | 8 characters | `routes/auth.js`, `lib/password-reset.js` |
| Inactivity before warning, then before digest | 30 days, then 30 more | `jobs/stale-account-*.js` |
| Marketplace page size | 50 (up to 100) | `routes/invites.js` |
| Rate limits | see API.md "Rate limits" | `routes/auth.js`, `routes/invites.js`, `routes/support.js` |
