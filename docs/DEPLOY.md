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

`apps/web/**`, `deploy/app/**`, and `db/migrations/**` are all CI trigger
paths, and the deploy job runs `git pull` on app-1 before touching Docker —
so pushing any of the above to `main` deploys itself. The manual steps
above are only needed for a first deploy, or to apply a new migration by
hand (CI never runs migrations automatically).

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

## 10. Stripe setup (once you're ready to bill)

1. Create the flat recurring Prices for each tier (free/starter/standard/
   plus/pro) in the Stripe dashboard.
2. Create a Billing Meter (`relay_gb_used`, aggregation: sum) and a metered
   Price with graduated tiers matching each plan's included-GB allowance.
3. Add a webhook endpoint pointing at
   `https://api.yourdomain.com/webhooks/stripe`, subscribed to at least
   `invoice.payment_succeeded`, `invoice.payment_failed`, and
   `customer.subscription.updated` — matching the handler in
   `apps/api/src/routes/stripe-webhook.js`.
4. Copy the webhook's signing secret into `.env` as `STRIPE_WEBHOOK_SECRET`.
