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

```bash
ssh relay-1-ip
sudo mkdir -p /opt/backup-buddies && sudo chown $USER /opt/backup-buddies
git clone git@github.com:yourusername/backup-buddies.git /opt/backup-buddies
cd /opt/backup-buddies/deploy/relay

# TLS: get a cert for relay.yourdomain.com (e.g. certbot, since this host
# is DNS-only and not behind Cloudflare's proxy) and point config.toml's
# manual_cert_path / manual_key_path at it, or check whether your installed
# iroh-relay version supports automatic cert issuance instead.

docker compose up -d
docker compose logs -f relay
```

## 7. Set up the GitHub Actions deploy

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

## 8. Stripe setup (once you're ready to bill)

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
