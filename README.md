# Backup Buddies

Peer-to-peer backup trading: users pledge storage to a paired "buddy" and back up
to each other's disks instead of a cloud provider. Files are chunked and
encrypted client-side before they ever leave a device — neither buddy, nor this
service, can read the other's data.

## How it's laid out

This is two independently deployable pieces, one per server:

| Path | Deploys to | What it is |
|---|---|---|
| `apps/api/` + `deploy/app/` | `app-1` | Control plane: Express API + Postgres + Caddy (TLS) |
| `apps/web/` | `app-1` (served by Caddy) | The site, dashboard and admin page (plain HTML/JS) |
| `deploy/relay/` | `relay-1` | Self-hosted Iroh relay (NAT-traversal fallback) |
| `apps/client/` | users' machines | The backup client (Rust, Docker image on GHCR) |

The API never touches user file data — it only handles accounts, buddy
pairing, device NodeIds, quotas, and billing. The relay only ever forwards
already-encrypted bytes between two paired clients when a direct P2P
connection can't be established.

## Stack

- **API**: Node.js (Express) + `pg` — see `apps/api`
- **DB**: Postgres 16, schema in `db/init.sql`
- **Relay**: official `n0computer/iroh-relay` Docker image
- **TLS / reverse proxy**: Caddy (automatic HTTPS)
- **DNS / edge**: Cloudflare (see note below — the relay needs a DNS-only record)
- **Billing**: Stripe (flat tier price + metered relay-usage price via Stripe
  Billing Meters)

## Cloudflare: where it fits and where it doesn't

Cloudflare's proxy ("orange cloud") is great in front of the **API** domain
(e.g. `api.yourdomain.com`) — free DDoS protection, caching for static
responses, and you already pay for it. Use it there.

**Do not proxy the relay's hostname through Cloudflare.** The relay needs raw
UDP/QUIC reachability for NAT hole-punching and direct connections; Cloudflare's
proxy only handles HTTP(S)/TCP traffic on specific ports and will break this.
Give the relay host (e.g. `relay.yourdomain.com`) a **DNS-only (grey cloud)**
record pointing straight at the relay server's IP.

Cloudflare is still useful for the relay's DNS record itself, and you can put
Cloudflare Access in front of anything admin-only later (e.g. a future
dashboard) without it interfering with the relay protocol.

## OS choice: Ubuntu 24.04 LTS, not "latest"

Use **Ubuntu 24.04 LTS** on both servers specifically, not whatever "latest"
resolves to at image-creation time. LTS releases get security patches for
years and won't change Docker/kernel behavior out from under you on a
re-provision; a non-LTS "latest" release goes end-of-life in ~9 months, which
you don't want to discover mid-production.

## Deploying

See `docs/DEPLOY.md` for the step-by-step: provisioning the two servers,
DNS records, environment variables, and the first `docker-compose up`.

## Reference

- `docs/API.md`: every API route, who can call it, what it takes and does.
- `docs/OPERATIONS.md`: servers, outside services, every setting,
  background jobs, scripts, CI, and routine commands.
- `apps/client/README.md`, "Commands and settings": the client's commands,
  `.env` settings, local dashboard endpoints and buddy protocol.

## Environment variables

Copy `.env.example` to `.env` on `app-1` and fill in real values. **Never
commit `.env`.**

## License

Copyright (C) 2026 Spencer LeBlanc.

- **`apps/client/`**, the program that runs on your machine: GPL-3.0-only,
  see [`apps/client/LICENSE`](apps/client/LICENSE). It's also mirrored on
  its own at
  [backup-buddies-client](https://github.com/mspencerl87/backup-buddies-client).
- **Everything else** (API, site, database schema, deploy files, scripts,
  docs): AGPL-3.0-only, see [`LICENSE`](LICENSE). If you run a modified
  version of the server for other people, the AGPL asks you to offer them
  your source.

Contributions are welcome; see [`CONTRIBUTING.md`](CONTRIBUTING.md).
