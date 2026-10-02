# Deploying

Two Linodes, matching `docs/architecture` from earlier planning: `app-1`
(control plane) and `relay-1` (Iroh relay).

## 1. Provision the Linodes

For both:

- **Image**: Ubuntu 24.04 LTS (not "latest" — see README for why)
- `app-1`: Linode 2GB plan
- `relay-1`: Nanode 1GB plan
- Add your SSH key during creation rather than a root password
- Enable Linode Backups on `app-1` (its Postgres data is the one thing here
  that isn't recoverable from anywhere else)

## 2. Lock down networking

Create a Cloud Firewall (Linode's dashboard, free) for each:

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

(Linode's marketplace also offers a one-click Docker image if you'd rather
provision with it already installed.)

## 5. First deploy — app-1

```bash
# on your machine, clone the private repo onto the server
ssh app-1-ip
sudo mkdir -p /opt/backup-buddies && sudo chown $USER /opt/backup-buddies
git clone git@github.com:yourusername/backup-buddies.git /opt/backup-buddies
cd /opt/backup-buddies/deploy/app

cp ../../.env.example .env
# edit .env: set a real POSTGRES_PASSWORD, DATABASE_URL with the same
# password, STRIPE_SECRET_KEY, STRIPE_WEBHOOK_SECRET, STRIPE_RELAY_METER_ID

# edit Caddyfile: replace api.yourdomain.com with your real domain

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
mkdir -p /opt/backup-buddies && cd /opt/backup-buddies
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
cd /opt/backup-buddies/deploy/relay && docker compose restart relay
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
cd /opt/backup-buddies/deploy/app
docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
  < ../../db/migrations/002_auth_and_invites.sql
```

**Every deploy after a pull:**

```bash
cd /opt/backup-buddies
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
cd /opt/backup-buddies && git pull
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
   optional) and restart the API so it picks up both the new env vars and
   the new `google-auth-library` dependency:
   ```bash
   cd /opt/backup-buddies/deploy/app
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     < ../../db/migrations/003_google_oauth.sql
   docker compose up -d --build api
   ```
6. Test it: visit `/login.html`, click **Continue with Google**, and
   confirm you land on the dashboard afterward. Signing in with a Google
   account whose email matches an existing password account links the two
   (same account, now usable either way) rather than creating a duplicate.

## 10. Stripe setup (billing by pledged storage)

Billing model: 50GB free per account, then $3/TB/month on however much
storage you're actively pledging across your buddy pairings — a
subscription **quantity** (in GB) that updates automatically whenever a
pairing is created or ends, not a live usage meter. Relay-bandwidth overage
billing (`STRIPE_RELAY_METER_ID`, `relay_usage_events`) is a separate,
not-yet-built piece — this section is just the storage subscription.

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
4. **Apply the migration and restart:**
   ```bash
   cd /opt/backup-buddies/deploy/app
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     < ../../db/migrations/005_billing.sql
   docker compose up -d --build api
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

1. **Apply the migration and restart:**
   ```bash
   cd /opt/backup-buddies/deploy/app
   docker compose exec -T postgres psql -U backup_buddies -d backup_buddies \
     < ../../db/migrations/006_devices_and_marketplace.sql
   docker compose up -d --build api
   ```
2. That same `git pull` + `docker compose up -d` already picks up the
   Caddyfile and docker-compose.yml changes that make `apps/client/`
   served publicly at `https://app.filegarden.net/client/` — this is how
   customers get the client without needing access to this (private)
   repo. Nothing else to do here; the client itself never runs on app-1,
   only its source files are served from it.

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
cd /opt/backup-buddies/deploy/app
# edit .env: add/confirm ADMIN_EMAILS=mspencerl87@gmail.com (or your real list)
docker compose up -d --build api
```

A few things worth knowing before using the delete actions:

- Deleting an account cascades in Postgres (devices, buddy_pairings,
  invites, sessions, relay_usage_events all reference `accounts` with
  `ON DELETE CASCADE`) — one click removes all of it from the database.
- It does **not** touch Stripe. A deleted account's subscription, if it
  has one, keeps running and billing until you cancel it separately in
  the Stripe dashboard — the UI warns on this, but doesn't do it for you.
- You can't delete your own (admin) account from the page — avoids a
  confusing self-lockout mid-troubleshooting.
