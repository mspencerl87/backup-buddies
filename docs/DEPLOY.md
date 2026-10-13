# Deploying

Two VPSes: `app-1` (control plane) and `relay-1` (Iroh relay).

Commands here use `$CHECKOUT` for where the repo is checked out on each
server. Set it once in the deploy user's shell profile (e.g.
`export CHECKOUT=/srv/backup-buddies` in `~/.bashrc`) so they paste as-is.

## 1. Provision the servers

For both:

- **Image**: Ubuntu 24.04 LTS (not "latest" — see README for why)
- `app-1`: 2GB RAM
- `relay-1`: 1GB RAM
- Add your SSH key during creation rather than a root password
- Turn on the provider's disk backups for `app-1` (its Postgres data is the
  one thing here that isn't recoverable from anywhere else)

## 2. Lock down networking

Create a firewall for each (the provider's cloud firewall, or `ufw`):

- `app-1`: allow inbound 22 (SSH, ideally restricted to your IP), 80, 443.
  Nothing else — Postgres is never exposed outside the Docker network.
- `relay-1`: allow inbound 22 (restricted), 443, and 7824/udp (see the
  caveat in `deploy/relay/docker-compose.yml` about confirming this port
  against the current iroh-relay docs before relying on it).

## 3. DNS (Cloudflare)

- `api.yourdomain.com` → `app-1`'s IP, **proxied** (orange cloud) — fine to
  route through Cloudflare, gets you free DDoS protection on the API.
- `app.yourdomain.com` → `app-1`'s IP, **proxied** (orange cloud) — this is
  the public site (landing page, signup, login, dashboard). Same IP as the
  `api` record, same proxy setting, just a second A record.
- `relay.yourdomain.com` → `relay-1`'s IP, **DNS only** (grey cloud) — must
  NOT be proxied, or NAT hole-punching breaks (see README).

## 4. Install Docker on both boxes

```bash
curl -fsSL https://get.docker.com | sh
```

## 5. First deploy — app-1

```bash
# on your machine, clone the private repo onto the server
ssh app-1-ip
sudo mkdir -p $CHECKOUT && sudo chown $USER $CHECKOUT
git clone git@github.com:yourusername/backup-buddies.git $CHECKOUT
cd $CHECKOUT/deploy/app

cp ../../.env.example .env
# edit .env: set a real POSTGRES_PASSWORD, DATABASE_URL with the same
# password, STRIPE_SECRET_KEY, STRIPE_WEBHOOK_SECRET, STRIPE_RELAY_METER_ID

# edit caddy/Caddyfile: replace the hostnames with your real domains

docker compose up -d
docker compose logs -f api   # confirm it started cleanly
curl https://api.yourdomain.com/healthz
```

## 6. First deploy — relay-1

Verified end-to-end against the real `n0computer/iroh-relay:v1.3.0` image on
2026-10-01 — `config.toml`'s fields and the port layout below are confirmed
working, not just best-guess from docs.

```bash
ssh relay-1-ip
mkdir -p $CHECKOUT && cd $CHECKOUT
git clone https://github.com/yourusername/backup-buddies.git .
cd deploy/relay
```

**Get a TLS cert.** This host is DNS-only (grey cloud) behind Cloudflare, so
nothing else is terminating TLS for it — `cert_mode = "Manual"` in
`config.toml` expects you to supply a real cert yourself via certbot:

```bash
# Port 80 must be open in the relay-1 firewall for this (temporarily, or
# permanently for renewals) — the ACME HTTP-01 challenge needs it.
apt update && apt install -y certbot
certbot certonly --standalone -d relay.yourdomain.com
```

This writes `fullchain.pem` and `privkey.pem` to
`/etc/letsencrypt/live/relay.yourdomain.com/` — `config.toml`'s
`manual_cert_path`/`manual_key_path` and the compose file's volume mount
both already expect exactly those filenames.

**Set up renewal to actually reach the running container.** Certbot renews
the cert automatically via its own systemd timer, but this container only
reads the cert file at startup — a renewal alone won't make it pick up the
new cert. Add a deploy hook that restarts the service on renewal:

```bash
cat > /etc/letsencrypt/renewal-hooks/deploy/restart-relay.sh << 'EOF'
#!/bin/sh
cd $CHECKOUT/deploy/relay && docker compose restart relay
EOF
chmod +x /etc/letsencrypt/renewal-hooks/deploy/restart-relay.sh
```

**Bring it up:**

```bash
docker compose up -d
docker compose logs -f relay
curl -I https://relay.yourdomain.com
```

## 7. Public site, accounts, and buddy pairing

This adds the signup/login/dashboard flow and the `app.yourdomain.com`
static site on top of the base deploy from step 5. It's a normal code
update, not a new server — do this on `app-1`.

**One-time: DNS.** Add the `app.yourdomain.com` record from step 3 in
Cloudflare if you haven't already (A record, same IP as `api`, proxied).

**One-time: database migration.** The new accounts/sessions/invites tables
live in `db/migrations/002_auth_and_invites.sql`. `init.sql` only runs
against a fresh, empty Postgres volume, so on an already-running database
you apply the migration by hand, once:

```bash
cd $CHECKOUT/deploy/app
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
  < ../../db/migrations/002_auth_and_invites.sql
```

**Every deploy after a pull:**

```bash
cd $CHECKOUT
git pull
cd deploy/app
docker compose up -d --build   # --build picks up the new npm deps in apps/api
docker compose logs -f api     # confirm it started cleanly
curl https://app.yourdomain.com/            # landing page
curl https://api.yourdomain.com/healthz     # API still healthy
```

What changed, for reference:

- `apps/api` gained `bcryptjs`, `cookie-parser`, and `express-async-errors`
  as dependencies, plus routes for `/signup`, `/login`, `/logout`, `/me`,
  `/invites`, and `/invites/:code/redeem` — all cookie-session-authenticated
  (see `apps/api/src/middleware/require-auth.js`).
- `apps/web/` is a new static site (landing page, signup, login, dashboard)
  with no build step — Caddy serves it directly.
- `deploy/app/Caddyfile` has a new `app.yourdomain.com` site block: `/api/*`
  proxies to the API container (prefix stripped), everything else is served
  as static files from `apps/web/`.
- `deploy/app/docker-compose.yml` mounts `apps/web` read-only into the
  `caddy` service at `/srv/web`.

`apps/api/**`, `apps/web/**`, `deploy/app/**`, and `db/migrations/**` are
all CI trigger paths (`.github/workflows/deploy.yml`), and the deploy job
runs `git pull` on app-1 before touching Docker — so pushing any of the
above to `main` deploys itself. The manual steps above are only needed for
a first deploy, or to apply a new migration by hand (CI never runs
migrations automatically).

**`apps/client/**` is deliberately *not* in that trigger list** — a client
change never needs an image build/push/restart on app-1, since the client
itself never runs there (see section 11). But Caddy on app-1 serves
`apps/client/` straight from this checkout (`deploy/app/docker-compose.yml`
mounts it read-only), so what `install.sh` and friends hand out to
customers is whatever's on disk here — **which CI will never update for
you**. After any push that touches `apps/client/`, pull it by hand:

```bash
cd $CHECKOUT && git pull
```

No `docker compose` command needed — Caddy just serves files off disk, so
the next request picks up the change immediately. Easy to forget: CI's own
`git pull` on app-1 only fires for the trigger paths above, so a
client-only push can sit live-but-unpulled indefinitely with nothing in
the Actions tab ever telling you — the first sign is usually a customer
(or you, testing `install.sh`) getting a build that doesn't match what's
in `main`.

## 8. Set up the GitHub Actions deploy

In the repo's Settings → Secrets and variables → Actions, add:

- `LINODE_APP_HOST` — app-1's IP or hostname
- `LINODE_APP_USER` — the SSH user you deploy as
- `LINODE_APP_SSH_KEY` — a private key whose public half is authorized on
  app-1 (consider a dedicated deploy key, not your personal one)

The workflows (`deploy.yml`, `build-client.yml`) `cd` into the checkout
by its full path; if your `$CHECKOUT` is somewhere else, change it there.

From then on, pushing to `main` with changes under `apps/api/` or
`deploy/app/` builds the image, pushes it to GHCR, and redeploys app-1
automatically. The relay side isn't wired into CI yet since it changes far
less often — redeploy it manually with `git pull && docker compose up -d`
when needed.

## 9. Google sign-in

Lets people sign up/log in with their Google account instead of (or in
addition to) email + password. Entirely optional — leave the env vars
unset and the "Continue with Google" button just redirects to a
"not configured" message, everything else keeps working.

1. In the [Google Cloud Console](https://console.cloud.google.com/), create
   a project (or use an existing one) and go to **APIs & Services →
   Credentials**.
2. **Configure the OAuth consent screen** first if you haven't: User type
   "External" is fine for a public product; add your support email; you
   don't need to submit for verification just to let people sign in with
   basic profile/email scopes.
3. **Create Credentials → OAuth client ID**, application type **Web
   application**.
   - Authorized JavaScript origins: `https://app.filegarden.net`
   - Authorized redirect URIs: `https://app.filegarden.net/api/auth/google/callback`
     — must match exactly, including the `/api` prefix (that's what Caddy
     strips before proxying to the API's `/auth/google/callback` route).
4. Copy the generated **Client ID** and **Client secret** into `.env` on
   app-1:
   ```
   GOOGLE_CLIENT_ID=xxx.apps.googleusercontent.com
   GOOGLE_CLIENT_SECRET=GOCSPX-xxx
   GOOGLE_REDIRECT_URI=https://app.filegarden.net/api/auth/google/callback
   ```
5. Apply the new migration (adds `google_sub`, makes `password_hash`
   optional), then restart — the `google-auth-library` dependency itself
   deploys automatically on push (`apps/api/**` is a CI trigger path), but
   the new env vars from step 4 only take effect on a restart, and CI has
   no reason to do one just because you edited `.env`. If that push is
   still fresh, check the Actions tab first and let CI's own deploy land
   before running this — restarting at the same moment races it for the
   `app-api-1` container name (see section 11's note):
   ```bash
   cd $CHECKOUT/deploy/app
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     < ../../db/migrations/003_google_oauth.sql
   docker compose up -d
   ```
6. Test it: visit `/login.html`, click **Continue with Google**, and
   confirm you land on the dashboard afterward. Signing in with a Google
   account whose email matches an existing password account links the two
   (same account, now usable either way) rather than creating a duplicate.

## 10. Stripe setup (billing by pledged storage)

Billing model: 50GB free per account, then $3/TB/month on however much
storage you're actively pledging across your buddy pairings — a
subscription **quantity** (in GB) that updates automatically whenever a
pairing is created or ends, not a live usage meter. Relay-bandwidth
billing is a separate piece, bundled onto the same subscription — see
section 13.

1. **Create the tiered Price.** Rather than clicking through the Stripe
   dashboard's pricing UI (fractional-cent-per-GB pricing is fiddly to get
   exactly right by hand), run the setup script against your Stripe secret
   key:
   ```bash
   STRIPE_SECRET_KEY=sk_test_xxx node scripts/create-stripe-storage-price.mjs
   ```
   No `npm install` needed — it talks to Stripe's REST API directly. It
   prints a `price_...` ID; put that in `.env` on app-1 as
   `STRIPE_STORAGE_PRICE_ID`. Start with your **test mode** secret key
   (Stripe dashboard → toggle Test mode → Developers → API keys) so you can
   run through a full signup → checkout → subscribe flow with fake cards
   before ever touching live payments.
2. **Set `APP_URL`** in `.env` to `https://app.filegarden.net` (no trailing
   slash) — the Checkout/Billing Portal success and return URLs are built
   from this.
3. **Add a webhook endpoint** (Stripe dashboard → Developers → Webhooks)
   pointing at `https://api.filegarden.net/webhooks/stripe`, subscribed to:
   `checkout.session.completed`, `customer.subscription.deleted`,
   `customer.subscription.updated`, `invoice.payment_succeeded`, and
   `invoice.payment_failed` — matching the handler in
   `apps/api/src/routes/stripe-webhook.js`. Copy its signing secret into
   `.env` as `STRIPE_WEBHOOK_SECRET`.
4. **Apply the migration, then restart to pick up the env vars from steps
   1-3** (the billing code itself already deployed on push — if that was
   just now, let CI's own deploy finish first; see section 11's note on
   why running this concurrently races it):
   ```bash
   cd $CHECKOUT/deploy/app
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     < ../../db/migrations/005_billing.sql
   docker compose up -d
   ```
5. **Test it end to end** (test mode): sign up or log into a test account,
   go to the dashboard's Billing card, click **Subscribe**, and use
   Stripe's test card `4242 4242 4242 4242`, any future expiry, any CVC. You
   should land back on the dashboard with "Subscription started," and the
   card should now say **Manage billing** (opens the Stripe-hosted Billing
   Portal — cancel, update payment method, view invoices, all handled by
   Stripe, not custom-built here). Create/redeem an invite between two test
   accounts afterward and confirm the subscription quantity in the Stripe
   dashboard updates to match the new pledge.
6. Only once all of that works in test mode: switch to your **live** secret
   key and webhook signing secret in `.env`, and re-run the setup script
   with the live key to create a live-mode version of the Price.

## 11. Device registration + the backup client

Adds device API tokens (so the headless Rust client can authenticate
without a browser session), a public marketplace for finding a buddy
without a direct invite code, and `apps/client/` — the actual P2P backup
agent customers run in Docker. This section is server-side setup; see
`apps/client/README.md` and the dashboard's **Set up the client** page
(`/setup.html`) for what the customer does on their own machine.

1. **Apply the migration:**
   ```bash
   cd $CHECKOUT/deploy/app
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     < ../../db/migrations/006_devices_and_marketplace.sql
   ```
   That's it — no manual restart needed. `apps/api/**` and `deploy/app/**`
   (the Caddyfile/docker-compose.yml changes that serve `apps/client/`
   publicly) are both CI trigger paths, so pushing this feature's code
   already deployed itself automatically; running `docker compose up -d`
   by hand too just races CI's own deploy for the `app-api-1` container
   name (see section 17's note on this — it's what broke a later
   deploy). Check the Actions tab or `docker compose logs api` if you
   want to confirm it's live rather than re-running anything.
2. Nothing else to do here; the client itself never runs on app-1, only
   its source files are served from it.

**Distributing the client:** since the repo is private, customers can't
`git clone` it. Two ways around that, both already wired up:

- **Default, zero-config:** `apps/client/install.sh` is served at
  `https://app.filegarden.net/client/install.sh` — customers run
  `curl -fsSL https://app.filegarden.net/client/install.sh | bash`, which
  downloads the source (docker-compose.yml, Dockerfile, Cargo files,
  main.rs) straight from your own server and builds the image locally.
  No GitHub access, no Docker registry login, nothing to misconfigure.
- **Faster, optional:** `.github/workflows/build-client.yml` also builds
  and pushes `ghcr.io/<repo>/client` on every push to `apps/client/**`.
  That GHCR package is private by default (matching the repo) — if you'd
  rather customers `docker pull` a prebuilt image instead of compiling
  Rust locally, make the `client` package public under the repo's
  Packages settings on GitHub, then switch the `image:` line in
  `apps/client/docker-compose.yml` back to the GHCR reference (it's left
  in as a comment).

**Where client data lives:** `apps/client/docker-compose.yml` bind-mounts
`DATA_DIR_HOST` (set in the customer's own `.env`, defaults to `./data`
next to their compose file) to `/data` in the container — a real,
customer-visible folder rather than a Docker-managed named volume, by
design (easier to find/move/back up independently). Nothing for app-1 to
do here; this only affects the customer's own machine.

## 12. Admin page

`/admin.html` lists every account (email, tier, device/pairing counts) and
lets you drill into one to see its devices, buddy pairings, and invites,
with delete actions on each — for troubleshooting, not a feature customers
see or know about.

Access is gated by `ADMIN_EMAILS` (comma-separated, case-insensitive) in
`.env` on app-1 — not a database role, since the admin set here is just
you. Unset or empty, `/admin/*` 403s for everyone, you included.

```bash
cd $CHECKOUT/deploy/app
# edit .env: add/confirm ADMIN_EMAILS=mspencerl87@gmail.com (or your real list)
docker compose up -d
```
(No `--build` — the admin page's code already deployed on whatever push
added it, via CI; this restart is only to pick up the env var. If you're
doing this right after that push, let CI's own deploy finish first —
running this at the same moment races it for the `app-api-1` container
name, see section 11's note.)

A few things worth knowing before using the delete actions:

- Deleting an account cascades in Postgres (devices, buddy_pairings,
  invites, sessions, relay_usage_events all reference `accounts` with
  `ON DELETE CASCADE`) — one click removes all of it from the database.
- It does **not** touch Stripe. A deleted account's subscription, if it
  has one, keeps running and billing until you cancel it separately in
  the Stripe dashboard — the UI warns on this, but doesn't do it for you.
- You can't delete your own (admin) account from the page — avoids a
  confusing self-lockout mid-troubleshooting.

## 13. Relay-bandwidth billing

Separate from storage billing (section 10): accounts with an active paid
subscription are also billed **$0.02/GB** for traffic that actually goes
through `relay.filegarden.net` — direct peer-to-peer transfers never touch
the relay and are never billed. Free-tier accounts are never billed for
relay bandwidth. The client reports its relay usage to the API roughly
every 30 seconds (piggybacked on its existing buddy-list poll — see
`apps/client/README.md`'s "Relay bandwidth" section); the API batches that
into Stripe Billing Meter events on a 15-minute schedule
(`apps/api/src/jobs/sync-relay-usage.js`).

1. **Create the Billing Meter + metered Price:**
   ```bash
   STRIPE_SECRET_KEY=sk_test_xxx node scripts/create-stripe-relay-meter-and-price.mjs
   ```
   Prints a `price_...` ID and a `mtr_...` ID. Put both in `.env` on app-1:
   `STRIPE_RELAY_PRICE_ID` (the one the running app actually reads — it's
   what gets added to checkout and what gates the sync job) and
   `STRIPE_RELAY_METER_ID` (kept for reference only; nothing reads it).
   Start in **test mode**, same reasoning as section 10.
2. **Restart the API** so it picks up the new env vars (no `--build` — the
   sync-job code already deployed on push, via CI; if that was just now,
   let CI's own deploy finish first, see section 11's note):
   ```bash
   cd $CHECKOUT/deploy/app
   docker compose up -d
   ```
   No new migration — `relay_usage_events` already exists (`db/init.sql`).
3. **Backfill existing subscribers**, if there are any accounts that
   subscribed to storage billing before this section was set up (new
   subscriptions get both the storage and relay prices from checkout
   automatically — see `apps/api/src/routes/billing.js`):
   ```bash
   STRIPE_SECRET_KEY=sk_test_xxx \
   STRIPE_STORAGE_PRICE_ID=price_xxx \
   STRIPE_RELAY_PRICE_ID=price_yyy \
   node scripts/add-relay-price-to-existing-subscriptions.mjs
   ```
4. **Test it end to end** (test mode): subscribe a test account, pair two
   test clients where at least one is forced through the relay (e.g. block
   direct UDP between them, or just trust that NAT traversal fails on
   whatever network you're testing from), let a backup cycle run, and
   confirm `relay_usage_events` rows show up
   (`docker compose exec -T postgres psql -U backup_buddies -d backup_buddies -c "select * from relay_usage_events order by occurred_at desc limit 5;"`)
   with `synced_to_stripe` flipping to `true` within 15 minutes, and a
   corresponding meter event appearing on the customer in the Stripe
   dashboard (Customers → the test customer → "Usage" / "Meter events").
5. Only once that works in test mode: switch to your **live** secret key
   and re-run both scripts with it (meters/prices are account-scoped, so
   test-mode ones don't carry over).

## 14. Outgoing email + email verification

Password signups now get a "verify your email" link (bot/throwaway-account
deterrent); Google signups skip it (Google already confirmed the address —
see `routes/auth.js`). Unverified accounts can still log in and look
around — only pairing (creating or redeeming an invite) is blocked, via
`requireEmailVerified` (`apps/api/src/middleware/require-email-verified.js`)
on those three routes in `routes/invites.js`.

Email is sent over plain SMTP (`apps/api/src/lib/mailer.js`, via
`nodemailer`) — no separate transactional-email provider needed if you
already have a mailbox. This was set up against **Purelymail**
(`smtp.purelymail.com`), but any standard SMTP mailbox works the same way.

1. **Set SMTP env vars** in `.env` on app-1:
   ```
   SMTP_HOST=smtp.purelymail.com
   SMTP_PORT=465
   SMTP_USER=you@yourdomain.com
   SMTP_PASS=...              # an app password if 2FA is on for that mailbox
   MAIL_FROM="Backup Buddies <you@yourdomain.com>"
   ```
   Port 465 is implicit TLS; use 587 for STARTTLS instead if your mailbox
   provider prefers that — `mailer.js` picks the right mode from the port
   number automatically. Leaving these unset doesn't break anything: the
   API just logs what it would have sent instead of erroring, which is
   fine for local dev but means no real emails go out in that state.
2. **Apply the migration, then restart to pick up the SMTP env vars from
   step 1** (the verification code itself already deployed on push — if
   that was just now, let CI's own deploy finish first; see section 11's
   note on why running this concurrently races it):
   ```bash
   cd $CHECKOUT/deploy/app
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     < ../../db/migrations/007_email_verification.sql
   docker compose up -d
   ```
   This backfills every *existing* account as already-verified — only new
   signups from here on need to click the link.
3. **Test it end to end**: sign up a new test account with email/password,
   confirm the verification email actually arrives (check spam too — a
   fresh sending domain has no reputation yet), click the link, and land on
   `/verify-email.html` showing "Email verified." Then confirm: (a) before
   clicking it, trying to create or redeem an invite returns the
   "verify your email" error with a link back to the dashboard's resend
   button; (b) the dashboard's resend button actually sends a new email
   (and that hammering it twice in a row gets the 60-second cooldown
   response instead of spamming the inbox).

## 15. Stale-account lifecycle

Flags accounts nobody's used in a while so they don't quietly keep billing
or holding a buddy's disk space forever — but **never deletes anything on
its own**. Two emails, both best-effort and logged on failure rather than
retried into a loop:

1. **Warning** (`jobs/stale-account-cleanup.js`, runs daily): any account
   with no device check-in (`devices.last_seen_at`) in 30 days gets an
   email saying so. If the account comes back to life (a client starts
   checking in again) before the next step, this clears itself
   automatically — no admin action needed either way at this stage.
2. **Digest** (`jobs/stale-account-digest.js`, runs roughly monthly): any
   account still showing no activity 30 days *after* its warning is
   listed, by email, address, days inactive, and whether it has a live
   Stripe subscription, in one email to every address in `ADMIN_EMAILS`.
   Deleting (and, separately, cancelling any subscription in the Stripe
   dashboard — `/admin.html`'s delete action doesn't do that part for you,
   same as any other admin delete) is then a deliberate action you take
   from `/admin.html`, same as using that page for any other reason.

Off by default — turning it on starts emailing real signups the moment
someone's inactive for a month, so don't flip it on until SMTP (section 14)
is actually working.

1. **Apply the migration** (no restart needed yet — `apps/api/**` is a CI
   trigger path, so the matching job code already deployed itself on
   push; see section 11's note on why running `docker compose up` by
   hand here too would just race it):
   ```bash
   cd $CHECKOUT/deploy/app
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     < ../../db/migrations/008_stale_account_cleanup.sql
   ```
2. **Exempt any accounts you want left alone regardless of activity** —
   e.g. `testa`/`testb` if you keep those around for troubleshooting
   (see section 12) — in `.env`:
   ```
   STALE_CLEANUP_EXEMPT_EMAILS=testa@example.com,testb@example.com
   ```
   Your own `ADMIN_EMAILS` accounts are always exempt automatically, no
   need to list them here too.
3. **Turn it on:**
   ```
   STALE_ACCOUNT_CLEANUP_ENABLED=true
   ```
   ```bash
   docker compose up -d
   ```
   (No `--build` needed — this is just picking up the env var; see
   section 11's note if you're doing this right after a fresh push.)
4. **Test it** (test mode, with a disposable test account): backdate that
   account's `created_at` and make sure it has no `devices` row (or an old
   `last_seen_at`) so it already qualifies as stale —
   ```bash
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     -c "UPDATE accounts SET created_at = now() - interval '35 days' WHERE email = 'your-test-account@example.com';"
   ```
   — then either wait for the next daily run or restart the API (it also
   runs 5 minutes after boot) and confirm the warning email arrives. To
   test the digest without waiting 30 more days, also backdate
   `stale_warning_sent_at`:
   ```bash
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     -c "UPDATE accounts SET stale_warning_sent_at = now() - interval '35 days' WHERE email = 'your-test-account@example.com';"
   ```
   then restart the API (it also runs 10 minutes after boot) and confirm
   the digest arrives at every `ADMIN_EMAILS` address, listing that
   account. Clean up the test account from `/admin.html` afterward.

## 16. Marketplace listing criteria (country, speed tier, always-on, relay-only)

Adds four optional, self-reported columns to `invites`
(`country`, `speed_tier`, `always_on`, `relay_only`), shown on "Find a
buddy" listings so a browser can judge fit beyond just the exact pledge
size they're already matched on — see `docs/TESTING-CHECKLIST.md`'s
"Marketplace listing criteria" design note for the full rationale. Also
adds `apps/web/find-a-buddy.html` as a standalone page (no new API routes
or env vars) — nothing to configure for that part, it's served the same
way every other static page under `apps/web` already is.

**Apply the migration before — or any time around — this deploys, not
strictly after:**
```bash
cd $CHECKOUT
git pull
cd deploy/app
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
  < ../../db/migrations/010_marketplace_listing_criteria.sql
```
Unlike some earlier migrations, there's no ordering requirement here and
no `docker compose up -d --build api` needed afterward — all four new
columns are additive with defaults/nullable, so the *currently running*
API (which has never heard of them) keeps working fine before you apply
it, and Postgres doesn't need a restart to pick up new columns. The only
failure mode is the reverse order: if the *new* API code (from this
deploy) is already running and queries these columns before they exist,
every `GET`/`POST /invites*` call 500s until you run the command above —
no restart needed once you do, it clears up on the next request. Applying
it early, before the code ships, avoids that window entirely.

## 17. Prevent duplicate buddy pairings

Nothing stopped two accounts from ending up with two active
`buddy_pairings` rows between them — e.g. each side sends the other an
invite code and both get redeemed. Each row looks individually fine (same
pledge either way), but a second row isn't a second relationship, it's
the same relationship pledged twice: it double-counts toward both sides'
50GB free-allowance checks and Stripe billing quantity, shows as a
confusing duplicate row on the dashboard, and gives the client two buddy
connections to reconcile against instead of one. `redeemInvite` in
`invites.js` now rejects redeeming a second invite between a pair that's
already actively paired (409, "you and this account are already paired
up"), and a new unique index closes the race a check-then-insert can't:

```bash
cd $CHECKOUT
git pull
cd deploy/app
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
  < ../../db/migrations/011_prevent_duplicate_pairings.sql
```

**That's the only manual step.** Do *not* also run
`docker compose up -d --build api` by hand here — `invites.js` is under
`apps/api/**`, a CI trigger path (section 7), so pushing this change to
`main` already deploys it automatically, usually within well under a
minute. Running the same `docker compose up` by hand at the same time
races the CI-driven one for the `app-api-1` container name and fails
both with "Error when allocating new name: Conflict" (this bit us live —
see the git history around this migration's commit if you want the
blow-by-blow). If you want to confirm the new code is live rather than
just trusting the race didn't happen, check the Actions tab or
`docker compose ps` / `docker compose logs api` instead of re-running the
deploy.

This migration changes existing data (revokes every pair's extra active
row(s) down to the oldest, then adds the unique index), but that's all
inside the `psql` step above — it doesn't interact with *when* the new
API code lands, so there's no ordering requirement against the CI
deploy either. No downtime: existing pairings keep working untouched (or,
for a duplicate, keep working as the one that survived), and nothing
about an active pairing's id, pledge, or history changes — only its
`status` on the rows being deduplicated.

## 18. Pause a paid account's buddy syncing when its payment fails

`relay_status` ('active'/'paused') has existed on `accounts` since the
very first schema, but until now nothing actually enforced it anywhere —
`invoice.payment_failed` set it to `'paused'` on the very first failed
charge, and no other code (API or client) ever read it. That's now wired
up end to end, per Spencer's requirement that a failed payment can't
silently keep incurring relay fees or silently never get noticed:

- `stripe-webhook.js` only pauses once Stripe's own retries are
  exhausted (`invoice.next_payment_attempt === null`), not on the first
  failed attempt — a card declined once and successfully retried a few
  days later (common — temporary holds, a bank's fraud check, etc.)
  never pauses anything. When it does pause, it also emails the account
  holder and stamps `relay_paused_at`/`relay_paused_reason`.
- `relay-billing.js`'s `recordRelayUsage` now refuses to record usage for
  a paused account — the actual backstop against more fees, independent
  of whether the client is behaving.
- `GET /devices/me/buddies` (`devices.js`) now returns a `paused` flag;
  the Rust client checks it every ~30s poll and — per "pause everything,
  not just relay" — skips backup, reconcile, and ping entirely for a
  paused account until it clears.
- The dashboard shows a banner when `relay_status !== 'active'`; the
  admin accounts page sorts paused accounts to the top (longest-paused
  first) and shows how long each has been paused, for manual follow-up —
  no auto-reminders or auto-deletion yet (see "Flagged, not yet scoped"
  in `docs/TESTING-CHECKLIST.md` if that changes).

Apply the migration (adds `relay_paused_at`/`relay_paused_reason`,
nullable with no default, so it's safe any time relative to the API
deploy):

```bash
cd $CHECKOUT
git pull
cd deploy/app
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
  < ../../db/migrations/012_relay_pause_tracking.sql
```

Same note as section 17: `apps/api/**` and `apps/web/**` are both CI
trigger paths, so pushing this to `main` deploys the API and web changes
automatically — don't also run `docker compose up -d` by hand here, or
you'll race the CI-driven deploy for the container name.

The Rust client change (`apps/client/src/main.rs`) is **not** part of
the CI deploy (`apps/client/**` isn't a trigger path — see section 7) —
existing running clients won't skip syncing on a pause until whoever's
running them pulls and rebuilds the client image themselves. Until then,
the server-side backstop in `relay-billing.js` is what actually stops
new fees from accruing; the client-side skip is the "stop bothering the
other buddy's device too" half of the fix.

## 19. Admin account-detail tools (password reset, Stripe panel)

The admin "View" card now shows the full account record, troubleshooting
buttons (send password reset link, mark email verified / resend
verification, sign out everywhere, pause / resume relay) and a live Stripe
section (customer, payment method, subscriptions, invoices, consistency
checks, relink / resync quantity / cancel). Also adds
`apps/web/reset-password.html` and `POST /auth/reset-password`.

**Apply the migration before this deploys:**
```bash
cd $CHECKOUT
git pull
cd deploy/app
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
  < ../../db/migrations/013_password_reset.sql
```
Additive and nullable, so the currently running API is unaffected. If the
new code ships first, everything works except the "Send password reset
link" button and the reset page, which 500 until the migration is in.

No new env vars. The Stripe panel uses the existing `STRIPE_SECRET_KEY`.
Password reset emails use the existing SMTP settings. Without SMTP, the
admin UI shows the reset link to paste into a support reply instead.
Admin actions are logged to the API container's stdout as
`[admin] <admin email> <action> account=<id>`.

## 20. Self-service password reset ("Forgot password?")

Adds "Forgot password?" on the login page (`forgot-password.html` →
emailed link → the same `reset-password.html` the admin button uses),
backed by `POST /auth/forgot-password` and `requestPasswordReset` in
`lib/password-reset.js`. Self-service and admin-issued links are the same
kind of token: hashed, single-use, 24-hour expiry, and a new link
invalidates any earlier one. A successful reset (either kind) now also
marks the email verified and emails a "your password was changed" notice;
device tokens are untouched, so running clients keep syncing.
Self-service specifics: the response is identical whether or not the
address is registered; a 60-second per-account cooldown plus a global cap
of 20 emails/minute stop the form being used to flood inboxes; Google-only
accounts get a "sign in with Google" email instead of a link (an admin can
still issue one from the account-detail view).

**No new migration.** It uses the columns from `013_password_reset.sql`
(section 19). If that's already applied, there's nothing manual to do here
— `apps/api/**`/`apps/web/**` are CI trigger paths, so pushing to `main`
deploys it. If it isn't applied yet, apply it as in section 19; until then
reset emails don't go out (the forgot-password page still shows its
generic message and the error is logged), and nothing else is affected.

Uses the existing `SMTP_*`/`MAIL_FROM`/`APP_URL` env vars. Optional:
`SUPPORT_EMAIL` overrides the support address shown in reset emails
(defaults to `support@filegarden.net`).

## 21. Cache-busted CSS/JS URLs

Every `href="/style.css"` / `src="/…js"` in `apps/web/*.html` now carries
`?v=<first 8 hex of the file's SHA-256>`. A changed stylesheet gets a new
URL, so browsers and Cloudflare can't keep serving a stale copy. Before
this, a style change could need Ctrl+Shift+R to show up. Unchanged files
keep their URL and stay cached.

The new `stamp-asset-versions` job in `.github/workflows/deploy.yml`
re-stamps on every deploy and commits the result back to `main` as
"Stamp asset versions" (from `github-actions[bot]`) before app-1 pulls.
Nobody has to remember to run it. **Pull before pushing after any CSS/JS
change**, since that bot commit will be on `main`. A push made with
`GITHUB_TOKEN` doesn't trigger workflows, so it can't loop.

To check or stamp by hand:
```bash
node scripts/stamp-asset-versions.mjs --check   # exit 1 if stale
node scripts/stamp-asset-versions.mjs           # rewrite in place
```
If `main` ever gets branch protection that blocks the bot's push, the job
warns instead of failing the deploy. Stamp by hand in that case.

## 22. In-app "Contact support" form

Adds a "Contact support" card to the dashboard, backed by `POST /support`
(`routes/support.js`). Logged-in users pick a category and write a
message; the API emails it to `SUPPORT_EMAIL` (default
`support@filegarden.net`) with the account's details attached and
`Reply-To` set to the user's address, so replying from the support inbox
goes straight to them. Nothing is stored. Limit: 5 requests per account per
hour.

**No migration, no new required env vars.** It uses the existing `SMTP_*`
settings; optional `SUPPORT_EMAIL` changes the destination. `apps/api/**`
and `apps/web/**` are CI trigger paths, so pushing to `main` deploys it.
If SMTP isn't configured, the form tells the user to email support
directly instead of pretending it sent.

Note for future routers: `supportRouter` is mounted *before* `adminRouter`
in `index.js` on purpose. `adminRouter` applies `requireAuth` +
`requireAdmin` to every request that reaches it, so anything mounted after
it is admin-only by accident.

## 23. Lock the relay to customers + free relay allowance

Until now relay-1 ran with `access = "everyone"`. Anyone running an Iroh
client could push traffic through it on our hosting bill, and free
accounts' relay use was unlimited. This section closes both:

- **Only registered devices can use the relay.** On every connection,
  iroh-relay POSTs the device's id to `POST /relay/access`
  (`routes/relay.js`). The API answers `true` only for a device registered
  to an account that isn't paused. A shared secret (`RELAY_ACCESS_TOKEN`)
  keeps anyone but relay-1 from calling it.
- **A hard per-device speed ceiling on the relay**: 4 MiB/s sustained,
  16 MiB bursts (`[limits.client.rx]` in `deploy/relay/config.toml`), plus
  a cap on new connections per second. This holds even against a modified
  client that lies about its usage.
- **Free accounts get 10 GB of relay a month** (`FREE_RELAY_GB_PER_MONTH`).
  Their usage is now recorded but never sent to Stripe. Past the allowance,
  the account pauses through the existing `relay_status` flag with reason
  `free_relay_cap`. The client stops syncing within ~30s, the user gets an
  email, and the dashboard says why. It resumes automatically on the 1st
  (UTC, `jobs/reset-free-relay-cap.js`), or immediately if they subscribe.
  Direct and LAN traffic never counts. Note that a paused free account also
  stops receiving its buddy's backups until it resumes.

**Do these in order.** Switching relay-1 over before the API knows the
token would deny every device.

1. Generate one token and keep it handy:
   ```bash
   openssl rand -hex 32
   ```
2. **app-1**: add it to `deploy/app/.env`, apply the index migration, and
   let the code deploy. Merging to `main` deploys the API; it changes
   nothing for the relay yet.
   ```bash
   cd $CHECKOUT/deploy/app
   echo 'RELAY_ACCESS_TOKEN=<token>' >> .env
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     < ../../db/migrations/014_relay_usage_by_month.sql
   # after the merge's CI deploy finishes (it restarts api with the new .env):
   docker compose up -d api
   ```
3. **Check it from relay-1** before touching the relay. Use a real node id
   from the admin page (a device's "Node id" → copy):
   ```bash
   curl -s -X POST https://api.filegarden.net/relay/access \
     -H "Authorization: Bearer <token>" -H "X-Iroh-NodeId: <node id>"; echo
   # → true
   curl -s -X POST https://api.filegarden.net/relay/access \
     -H "Authorization: Bearer <token>" \
     -H "X-Iroh-NodeId: 0000000000000000000000000000000000000000000000000000000000000000"; echo
   # → false
   ```
   Don't continue until the first one says `true`.
4. **relay-1**: pull, add the same token, restart:
   ```bash
   cd $CHECKOUT && git pull
   cd deploy/relay
   echo 'RELAY_ACCESS_TOKEN=<token>' > .env && chmod 600 .env
   docker compose up -d
   docker compose logs --tail 50 relay
   ```
   Devices reconnect on their own within ~16 seconds. In the client
   dashboards, buddies that were relaying should come back on their next
   cycle.

**Rolling back** is the same file with `access = "everyone"` (or
`git checkout` the old `config.toml`) and `docker compose up -d` on
relay-1. Nothing else depends on it.

**Good to know:**
- The relay only checks on connect. A device that's already connected
  stays connected until it reconnects. The client-side pause and the speed
  ceiling cover that gap.
- If the API is down, the relay refuses new connections. Devices that
  can connect directly keep syncing; relayed ones wait for the API.
- Denied devices retry about every 16 seconds, so a stranger's or deleted
  device's attempts show up as repeated 403s on `/relay/access` in the API
  logs. That's expected.
- A brand-new device is denied for its first few seconds, because the
  client opens its relay connection just before it registers its node id.
  iroh retries and gets in automatically.

## 24. Rate limiting on login, signup, password reset and invites

`lib/rate-limit.js`: a small in-memory limiter with no dependency and no
shared store, since there's one API process. A restart resets the counters,
which is harmless. Over a limit, the request gets a 429 with `Retry-After`
and a "try again in N minutes" message, which every form already shows.

| Endpoint | Limit | Keyed by |
|---|---|---|
| `POST /login` | 20 / 15 min | IP |
| `POST /login` | 10 / 15 min | email being signed into |
| `POST /signup` | 5 / hour | IP (protects the SMTP mailbox from bot signups) |
| `POST /auth/forgot-password` | 5 / 15 min | IP (on top of its per-account cooldown and global cap) |
| `POST /auth/reset-password` | 10 / 15 min | IP |
| `POST /auth/verify-email` | 20 / 15 min | IP |
| `POST /invites/…/redeem` | 20 / hour | account (stops guessing invite codes) |

Device clients (`/devices/*`) and relay-1 (`/relay/access`) are never
limited. When a limit trips, the API logs one `[rate-limit] …` line per key
per window, so `docker compose logs api | grep rate-limit` shows who hit what.

**No migration, no env vars.** Merging to `main` deploys it.

**The real visitor IP** comes from Caddy as `X-Client-IP` since §31: Caddy
trusts Cloudflare's `CF-Connecting-IP` only on requests from Cloudflare's
ranges, so someone who finds app-1's real IP and skips Cloudflare can no
longer fake it. (Before §31 the API read `CF-Connecting-IP` itself, which
could be faked that way.) Login also has a per-email limit, and invite
redemption is per-account, which never depended on the IP.

As an extra layer (optional), restrict app-1's inbound 80/443 in its
firewall to Cloudflare's published ranges
(https://www.cloudflare.com/ips/). Afterwards, confirm Caddy can still renew
its certificates. Watch `docker compose logs caddy` around the next renewal,
or run a forced renewal, since a challenge that can't get through would
eventually expire the cert.

**Side effect to know about:** the per-email login limit means someone
spamming wrong passwords for a user's email can lock that user out of
password login for up to 15 minutes. "Continue with Google" and password
reset still work for them in the meantime.

## 25. Client version reporting + in-place updates

Clients 0.2.0+ send `User-Agent: backup-buddies-client/<version>`. The API
stores it per device (`devices.client_version`), and the dashboard's Devices
card compares it to `apps/client/Cargo.toml` (served by Caddy at
`/client/Cargo.toml`). Outdated devices, including every pre-0.2.0 client,
since those don't report a version, get an "update available" badge and a
copy-able update command. The admin account view shows each device's
version too.

Updating is now in place, via `install.sh --update`:
```bash
curl -fsSL https://app.filegarden.net/client/install.sh | bash -s -- --update
```
It handles both kinds of install, and never touches `.env` or the data
folders:
- **Image installs** (the setup page's compose file running the published
  `ghcr.io/…/client:latest` image): `docker compose pull && docker compose
  up -d`. That's also what the setup page tells those users to run.
- **Source installs** (made by `install.sh`): replaces only the code and
  rebuilds.

Correction to the first version of this section: it said the setup page's
`docker compose pull` advice "did nothing because the client is built from
source". That was wrong. The setup page installs the prebuilt image, and
for those installs `pull` is the correct update. The first `--update` also
only understood source installs, so for image installs it refused with "no
existing install found". It refused safely, but uselessly. Both are fixed.

**Migration** (apply any time; the code works with or without it, and
versions just aren't recorded until it's in):
```bash
cd $CHECKOUT/deploy/app
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
  < ../../db/migrations/015_device_client_version.sql
```

**Releasing a client change from now on:** bump `version` in
`apps/client/Cargo.toml` (and the matching `Cargo.lock` entry), push, then
`git pull` on app-1 if nothing else triggered a deploy (section 7). Without
the version bump, nobody is told to update. See apps/client/README.md,
"Updating, and releasing a new version".

## 26. Nightly off-site database backup (Backblaze B2)

The provider's disk backups snapshot app-1, but they're deleted along with the
server and depend on the same account. Since client 0.7.0 the account id
is part of every user's encryption key, so losing the database would also
stop users restoring onto a new machine. `deploy/app/backup-db.sh` dumps
Postgres every night and uploads it to B2.

**One-time setup in Backblaze:**
1. Create a **private** bucket (e.g. `backup-buddies-db`). Under its
   settings, turn on **Default Encryption** (SSE-B2).
2. **Lifecycle Settings → custom rule:** prefix `postgres/`, hide files
   **30** days after upload, delete **1** day after hiding. That's the
   retention; the server never deletes anything itself.
3. **App Keys → Add a New Application Key:** allow access to that bucket
   only, type **Read and Write**, then note the keyID and applicationKey
   (shown once). Read-and-write includes delete; if you create keys with
   the B2 CLI instead, `listBuckets,listFiles,writeFiles` is enough.

**One-time setup on app-1:**
```bash
cd $CHECKOUT && git pull
cd deploy/app
# Add to .env: B2_KEY_ID, B2_APP_KEY, B2_BUCKET (and BACKUP_PING_URL below)
chmod +x backup-db.sh
./backup-db.sh          # first run by hand: should print "uploaded backup_buddies-….sql.gz"
sed "s|@CHECKOUT@|$CHECKOUT|" bb-db-backup.service > /etc/systemd/system/bb-db-backup.service
cp bb-db-backup.timer /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now bb-db-backup.timer
systemctl list-timers bb-db-backup.timer   # shows the next run (03:30 UTC)
```

**Alert if a night is missed:** in Uptime Kuma, add a monitor of type
**Push**, heartbeat interval 90000 seconds (25 h). Copy its push URL into
`BACKUP_PING_URL` in `.env`. The script pings it only after a successful
upload, so a failed or skipped backup raises an alert the next day.

**Logs:** `journalctl -u bb-db-backup.service -n 50`.

**Restore** (test this once now, into a scratch database, so it's known to
work before it's needed):
```bash
cd $CHECKOUT/deploy/app
# List backups and download one (fill in the key, id and bucket):
docker run --rm -e RCLONE_CONFIG_B2_TYPE=b2 -e RCLONE_CONFIG_B2_ACCOUNT=<keyID> \
  -e RCLONE_CONFIG_B2_KEY=<applicationKey> rclone/rclone:1.68.2 ls b2:<bucket>/postgres/
docker run --rm -v /tmp:/out -e RCLONE_CONFIG_B2_TYPE=b2 -e RCLONE_CONFIG_B2_ACCOUNT=<keyID> \
  -e RCLONE_CONFIG_B2_KEY=<applicationKey> rclone/rclone:1.68.2 \
  copy b2:<bucket>/postgres/<file>.sql.gz /out/

# Test restore into a scratch database, check it, drop it:
docker compose exec -T postgres createdb -U backup_buddies restore_test
gunzip -c /tmp/<file>.sql.gz | docker compose exec -T postgres psql -q -U backup_buddies -d restore_test
docker compose exec -T postgres psql -U backup_buddies -d restore_test -c "SELECT count(*) FROM accounts;"
docker compose exec -T postgres dropdb -U backup_buddies restore_test
```

For a real restore, stop the API (`docker compose stop api`), drop and
recreate `backup_buddies` the same way, load the dump into it, then
`docker compose start api`. The dump includes every migration applied at
the time, so nothing needs re-running.

## 27. Open-source client mirror (GPL-3.0)

The client (`apps/client`) is GPL-3.0-only and published at
`github.com/mspencerl87/backup-buddies-client`, with its own history
starting from one initial commit. Development stays in this private repo;
`.github/workflows/sync-public-client.yml` copies `apps/client` over on
every change to it, as one commit with the same subject. Nothing else from
this repo is published.

**One-time setup (deploy key):**
```bash
ssh-keygen -t ed25519 -N "" -C "sync from private repo" -f bb-public-sync
gh repo deploy-key add bb-public-sync.pub --repo mspencerl87/backup-buddies-client --allow-write --title "sync from private repo"
gh secret set PUBLIC_CLIENT_DEPLOY_KEY --repo mspencerl87/backup-buddies < bb-public-sync
rm bb-public-sync bb-public-sync.pub
```
Then run the workflow once (Actions → Sync client to public repo → Run
workflow) to confirm it pushes.

Before anything new lands in `apps/client`, remember it goes public: no
secrets, test-device details or references to private docs there.

## 28. Complimentary (billing-exempt) accounts

`accounts.billing_exempt`, toggled from an account's detail view in the
admin page ("Make complimentary" / "Remove complimentary", logged as
`[admin] … billing-exempt`). For test and friend accounts that should never
need a card:
- Pledges past the 50GB free allowance don't need a subscription (both when
  creating an invite and when a pairing is accepted). The other side of a
  pairing is still checked on its own.
- Relay usage is recorded but never sent to Stripe, and the free monthly
  relay cap never pauses them. Granting it lifts a `free_relay_cap` pause;
  other pauses (payment failed, admin) stay.
- The dashboard's Billing card says "Complimentary account — no billing"
  instead of Subscribe.

Manual only: being in `ADMIN_EMAILS` does not make an account exempt. It
doesn't touch Stripe either; an existing subscription keeps billing for
storage until it's cancelled from the admin Stripe section.

**Migration** (apply on app-1 **before** pushing the code; the API reads
the column directly, so invites, relay reports and the admin page fail
until it exists):
```bash
cd $CHECKOUT/deploy/app
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
  < ../../db/migrations/016_billing_exempt.sql
```
The file isn't on app-1 until the push, so copy it over first (or paste the
one `ALTER TABLE` line into `psql`).

## 29. Pre-launch site gate (removed)

From 2026-10-11 to 2026-10-12, Caddy put `app.filegarden.net` behind a
password (except `/client/*`) and returned `403` for `/signup` and the
Google sign-in routes on `api.filegarden.net`, to close signups during the
architecture rework. Reverted: the Caddyfile and the `caddy` service are
back to how they were. After the deploy, delete `SITE_USERNAME` and
`SITE_PASSWORD_HASH` from `deploy/app/.env`; nothing reads them.

## 30. Flat $3/month plan, relay allowance on both plans

The paid plan is now a flat $3/month for any pledge size instead of
$3/TB/month on pledged GB (`PLAN_USD_PER_MONTH` in `routes/billing.js`),
and paid accounts get the same 10GB/month of relay included as free ones,
billed only past it (`recordRelayUsage` in `lib/relay-billing.js`). The free
tier is unchanged: 50GB pledged, 10GB/month relay, then a pause. Nothing
syncs the Stripe quantity on pairing changes any more; the admin page's
"Resync quantity" is replaced by "Move to flat plan". No migration.

**Before pushing** (otherwise new checkouts go out on the old tiered price
with quantity 1, which charges $0):
1. Create the flat price (test key first if you want to try it there):
   ```bash
   STRIPE_SECRET_KEY=sk_live_… node scripts/create-stripe-storage-price.mjs
   ```
2. On app-1, note the current `STRIPE_STORAGE_PRICE_ID` in
   `deploy/app/.env` (that's the old tiered price; step 4 needs it), then
   replace it with the new id. The push's `docker compose up -d` recreates
   the API with it.

**After the deploy:**
3. Check the dashboard Billing card reads "Free plan: … the paid plan is
   $3/month" and the pricing page shows the new copy.
4. Move existing subscribers. Dry run first: it lists each live subscription
   on the old price with what it pays now against $3, and flags who goes up
   (anyone under about 1.07TB pledged):
   ```bash
   STRIPE_SECRET_KEY=sk_live_… OLD_STORAGE_PRICE_ID=price_old \
     STRIPE_STORAGE_PRICE_ID=price_new node scripts/move-subscriptions-to-flat-plan.mjs
   ```
   Tell the ones going up before they renew, then rerun with `--apply`. It
   swaps each subscription's item in place with no proration, so the period
   they've paid for stands and $3 applies from their next renewal.
   (Run from a machine with Node 20; the scripts need no `npm install`.)
5. Archive the old tiered price in the Stripe dashboard so nothing can
   subscribe to it by accident.

## 31. Security headers and a real client IP from Caddy

`deploy/app/caddy/Caddyfile` (`deploy/app/Caddyfile` until §32) now:
- trusts `CF-Connecting-IP` only on requests from Cloudflare's published
  ranges (`trusted_proxies` + `client_ip_headers`, global options) and sends
  the result to the API as `X-Client-IP`, overwriting anything a caller
  sent. `lib/rate-limit.js` keys per-IP limits on it, so going around
  Cloudflare to app-1's IP no longer lets anyone pick their own IP;
- adds `X-Frame-Options: DENY` and `frame-ancestors 'none'` (no page can be
  shown inside another site's frame), `X-Content-Type-Options: nosniff` and
  a `Referrer-Policy` to every response on both hostnames.

The API also answers a malformed id in a URL or session cookie with
`400`/`401` instead of a `500` (Postgres 22P02).

**Applied on 2026-10-12** with `docker compose up -d --force-recreate caddy`:
a plain `caddy reload` kept serving the old Caddyfile (see §32 for why and
the fix). Until Caddy has the new config, the API falls back to reading
`CF-Connecting-IP` itself, as before. Check:
```bash
curl -sI https://app.filegarden.net/ | grep -iE 'x-frame-options|nosniff|frame-ancestors'
```

**Cloudflare's ranges change rarely.** If they do, update the list in
`caddy/Caddyfile` from https://www.cloudflare.com/ips/ and push (§32
reloads it); a missing range just means visitors arriving through it are
keyed by the Cloudflare edge address until then.

## 32. Caddyfile in its own folder, reloaded by CI

The Caddyfile was bind-mounted as a single file. `git pull` replaces a
changed file rather than editing it in place, and a single-file bind mount
stays attached to the old copy, so the container kept seeing (and
`caddy reload` kept applying) the stale Caddyfile: §31's headers only took
effect after recreating the container. Now:
- it lives at `deploy/app/caddy/Caddyfile`, and compose mounts the folder
  (`./caddy:/etc/caddy:ro`), so a pull is visible inside the container;
- `deploy.yml` runs `caddy reload` after `docker compose up -d` on every
  deploy (retried a few times while a just-recreated Caddy starts). It's a
  no-op when nothing changed, and an invalid Caddyfile is refused (the
  running config stays) and fails the job.

**Nothing to do by hand.** The first deploy with this change recreates the
`caddy` container itself, because its mount changed: a second or two
without the site.

## 33. Three upload speed tiers, filterable on Find a buddy

"Find a buddy" gets an "Upload speed" filter (Any / 10 Mbps or more /
50 Mbps or more, `?min_speed_tier=` on `GET /invites/public`), and the
listing speed tiers go from four to three: `under_10`, `10_50`, `50_plus`.
Migration 017 rewrites existing `50_200` and `200_plus` listings to
`50_plus`.

**Migration** (apply on app-1 **before** pushing the code; the new API
rejects the old values and would hide those listings from the speed
filter until it runs). The old API only displays `speed_tier`, so running
it early is harmless:
```bash
cd $CHECKOUT/deploy/app
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies   < ../../db/migrations/017_three_speed_tiers.sql
```
The file isn't on app-1 until the push, so copy it over first (or paste
the one `UPDATE` line into `psql`). No new env vars.
