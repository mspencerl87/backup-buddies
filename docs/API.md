# Backup Buddies API reference

Every HTTP route the API (`apps/api`) serves, who can call it, what it
takes and what it does. The code in `apps/api/src/routes/` is the source
of truth; each route there has a comment explaining *why* it works the way
it does. This file is the map. Keep it in step when a route changes.

Related: [OPERATIONS.md](OPERATIONS.md) (env vars, background jobs,
scripts, servers), [DEPLOY.md](DEPLOY.md) (setup history, one section per
feature). The client's own commands, settings and local dashboard
endpoints are in `apps/client/README.md` ("Commands and settings").

## Contents

- [Base URLs](#base-urls)
- [Authentication](#authentication)
- [Conventions](#conventions)
- [Rate limits](#rate-limits)
- [Health](#health)
- [Accounts and sign-in](#accounts-and-sign-in)
- [Your account](#your-account)
- [Devices (browser side)](#devices-browser-side)
- [Devices (client side)](#devices-client-side)
- [Invites and pairing](#invites-and-pairing)
- [Marketplace](#marketplace)
- [Billing](#billing)
- [Support](#support)
- [Relay access (relay-1 only)](#relay-access-relay-1-only)
- [Stripe webhook](#stripe-webhook)
- [Admin](#admin)
- [Things there is no route for](#things-there-is-no-route-for)

## Base URLs

The same Express app is reachable two ways (see `deploy/app/Caddyfile`):

| URL | Used by | Path |
|---|---|---|
| `https://app.filegarden.net/api/...` | The website (browser) | Caddy strips `/api`, so `/api/signup` reaches the route `POST /signup` |
| `https://api.filegarden.net/...` | Backup clients (`API_URL`), relay-1 | Unprefixed: `/devices/me` reaches `GET /devices/me` |

Paths below are written without the `/api` prefix. From a browser, add it.

## Authentication

There are five ways to authenticate. Each route below says which one it uses.

| Name | How | Set up by |
|---|---|---|
| **Session** | `session_id` cookie (httpOnly, secure, SameSite=Lax, 30 days). Missing → `401 not authenticated`; expired → `401 session expired or invalid` | `POST /signup`, `POST /login`, Google sign-in |
| **Session + verified** | Session, and the account's email is verified. Otherwise `403` with `email_unverified: true` | Clicking the emailed link, or signing in with Google |
| **Admin** | Session, and the account's email is in `ADMIN_EMAILS` (env, comma-separated, case-insensitive). Otherwise `403 admin access required`. It's an allowlist, not a database role | `.env` on app-1 |
| **Device token** | `Authorization: Bearer <device token>`. Only the SHA-256 is stored. Missing → `401 missing device token`, unknown → `401 invalid device token` | `POST /devices` (shown once, in the dashboard) |
| **Relay token** | `Authorization: Bearer <RELAY_ACCESS_TOKEN>` | Same value in `deploy/app/.env` and `deploy/relay/.env` |
| **Stripe signature** | `Stripe-Signature` header checked against `STRIPE_WEBHOOK_SECRET` | Stripe dashboard webhook endpoint |

## Conventions

- Request and response bodies are JSON (`Content-Type: application/json`),
  except the Stripe webhook (raw body) and `/relay/access` (plain-text
  response).
- Errors are `{ "error": "<human-readable message>" }`, worded to be shown
  to the user as-is.
- An unhandled exception gives `500 {"error": "internal server error"}`
  (logged). Admin routes instead return the real message, prefixed with
  `Stripe:` when Stripe raised it: `400` if the upstream error was a 4xx,
  otherwise `502`.
- Sizes: `pledged_bytes` is bytes, on a GB basis of 1024³. Pledges must be
  a multiple of **5 GB**, from 5 GB to 20,000 GB.
- **Free allowance:** the first **50 GB** pledged across an account's active
  pairings is free (`FREE_GB` in `routes/billing.js`). Going past it needs
  a Stripe subscription, unless an admin has made the account
  **complimentary** (`billing_exempt`).

## Rate limits

In-memory and per API process, so a restart resets them. Going over gives
`429` with a `Retry-After` header. The client IP is taken from
`CF-Connecting-IP` (Cloudflare). Device and relay routes are never limited.

| Route | Limit | Keyed on |
|---|---|---|
| `POST /login` | 20 per 15 min, and 10 per 15 min | IP, and the email being signed into |
| `POST /signup` | 5 per hour | IP |
| `POST /auth/forgot-password` | 5 per 15 min, plus a 60 s per-account cooldown and a global per-minute cap (lib/password-reset.js) | IP |
| `POST /auth/reset-password` | 10 per 15 min | IP |
| `POST /auth/verify-email` | 20 per 15 min | IP |
| `POST /auth/resend-verification` | 60 s cooldown per account | account |
| `POST /invites/:code/redeem`, `POST /invites/public/:id/redeem` | 20 per hour (shared) | account |
| `POST /support` | 5 per hour | account |

## Health

| Route | Auth | Response |
|---|---|---|
| `GET /healthz` | none | `200 {"status":"ok"}` if the process is up. Doesn't touch the database |
| `GET /readyz` | none | `200 {"status":"ok"}` if Postgres answers `SELECT 1`, otherwise `503 {"status":"error","detail":"database unreachable"}` |

## Accounts and sign-in

All in `routes/auth.js`.

### `POST /signup`
Auth: none. Creates a password account, signs it in (sets the session
cookie), and sends a verification email (best effort: a failed send
doesn't fail the signup).

| Body field | Required | Notes |
|---|---|---|
| `email` | yes | Trimmed and lowercased |
| `password` | yes | At least 8 characters (bcrypt, cost 12) |
| `accepted_terms` | yes | Must be exactly `true`. Stored as `terms_accepted_at` |

`201 {"account": {id, email, tier, created_at, email_verified:false}}`.
`400` on a missing field, short password or terms not accepted. `409` if
the email is already registered.

### `POST /login`
Auth: none. Body `{email, password}`. Sets the session cookie.
`200 {"account": {id, email, tier}}`, or `401 invalid email or password`.
A missing account takes as long as a wrong password, so the timing
doesn't reveal which emails are registered. A Google-only account has no
password and always gets `401` here.

### `POST /logout`
Auth: session. Deletes this session (other sessions stay) and clears the
cookie. `200 {"ok":true}`.

### `GET /auth/google`
Auth: none. Browser navigation, not fetch. Sets a 10-minute `oauth_state`
cookie and redirects to Google's consent screen. If `GOOGLE_CLIENT_ID`,
`GOOGLE_CLIENT_SECRET` or `GOOGLE_REDIRECT_URI` isn't set, it redirects to
`/login.html?error=google_not_configured`.

### `GET /auth/google/callback`
Auth: none. Google redirects here (`GOOGLE_REDIRECT_URI`). It checks the
state, swaps the code for an ID token and verifies it. Then it signs in by
Google subject. If none matches, it links an existing account with the
same email (marking it verified and recording terms acceptance). Otherwise
it creates a new, already-verified account. Then it sets the session and
redirects to `/dashboard.html`. Failures redirect to `/login.html?error=`
with one of: `google_not_configured`, `google_state_mismatch`,
`google_token_exchange_failed`, `google_token_invalid`,
`google_email_unverified`.

### `POST /auth/verify-email`
Auth: none (the token is the credential). Body `{token}` from the emailed
link (`verify-email.html`). Tokens last 48 hours and work once.
`200 {"ok":true}`, `410` if expired, `400` if invalid.

### `POST /auth/resend-verification`
Auth: session. Sends a new verification link. `200 {"ok":true}`, `409` if
already verified, `429` if one was sent in the last 60 s.

### `POST /auth/forgot-password`
Auth: none. Body `{email}`. Always answers `200` with the same message,
whether or not the account exists, so the form can't be used to find out
who's registered. The email goes out in the background. Links last 24
hours and work once. `400` only if `email` is missing.

### `POST /auth/reset-password`
Auth: none (the token is the credential). Body `{token, password}`. Sets
the new password, **deletes every session for the account**, and clears
this browser's cookie. Also works for Google-only accounts, which gives
them a password. `200 {"ok":true}`; `400` for a weak password (under 8),
or an invalid or used link; `410` if expired.

## Your account

`routes/me.js`.

### `GET /me`
Auth: session. What the dashboard loads first.

```json
{
  "account": {
    "id", "email", "nickname", "tier", "has_subscription", "relay_status",
    "relay_paused_reason", "created_at", "has_password", "has_google",
    "is_admin", "email_verified"
  },
  "devices": [{ "id", "iroh_node_id", "label", "token_last_four", "created_at", "last_seen_at" }],
  "pairings": [{ "id", "account_a_id", "account_b_id", "pledged_bytes", "status", "created_at" }]
}
```

- `tier` is set at signup and never updated. Use `has_subscription` to
  tell whether an account is paying.
- `relay_status` is `active` or `paused`. `relay_paused_reason` is
  `payment_failed`, `free_relay_cap`, or an admin's note.
- `pairings` lists only active pairings.

### `PATCH /me`
Auth: session. Body `{nickname}`: the name shown on marketplace listings
instead of your email. Up to 40 characters; `""` or `null` clears it.
`200 {"ok":true}`.

## Devices (browser side)

`routes/devices.js`. Managing your own devices from the dashboard.

### `POST /devices`
Auth: session. Body `{label?}`. Creates a device and issues it a token.
`201 {"device": {id, label, created_at, token_last_four}, "token": "<64 hex>"}`.
**The token is only ever returned here.** The user pastes it into the
client's `.env` as `DEVICE_TOKEN`. A lost token can't be recovered:
delete the device and add a new one.

### `GET /devices`
Auth: session. `{"devices": [{id, iroh_node_id, label, token_last_four,
created_at, last_seen_at, client_version}]}`. `client_version` comes from
the client's User-Agent, and is `null` for clients older than 0.2.0.

### `DELETE /devices/:id`
Auth: session. Deletes one of your own devices, which revokes its token.
`204`, or `404` if it isn't yours.

## Devices (client side)

`routes/devices.js`. Called by the backup client with its device token.
The client sends `User-Agent: backup-buddies-client/<version>`.

### `GET /devices/me`
Auth: device token. `{"device_id", "account_id", "label"}`. The client
salts its encryption key with `account_id`, so all of an account's devices
derive the same key from the same passphrase.

### `PUT /devices/me/node-id`
Auth: device token. Body `{iroh_node_id}`. Called at client startup to
register this device's Iroh node id. It also updates
`last_seen_at`. `200 {"ok":true}`; `400` if missing; `409` if another
device row already has that node id (usually a stale device left over
from an earlier `.env` mix-up: delete it in the dashboard).

### `GET /devices/me/buddies?relay_mb=<n>`
Auth: device token. The client's ~30-second poll. It:
1. updates `last_seen_at` (which also counts as activity for the
   stale-account job), and records the client version if it changed;
2. if `relay_mb` (an integer above 0) is given, records that many relayed
   megabytes **since the last successful report**. Capped at 100,000 per
   report. Free accounts are capped at `FREE_RELAY_GB_PER_MONTH`.
   Subscribed ones are billed. Complimentary ones are never billed or
   capped (`lib/relay-billing.js`). Nothing is recorded while the account
   is paused;
3. returns:

```json
{
  "buddy_devices": [{ "id", "account_id", "iroh_node_id", "label", "pledged_bytes" }],
  "paused": false,
  "self_label": "NAS in the closet"
}
```

`buddy_devices` lists every device, with a node id registered, on every
account this account has an active pairing with. `paused: true` tells the
client to stop all buddy activity (payment failed, free relay allowance
used up, or an admin hold).

## Invites and pairing

`routes/invites.js`. An invite is an offer: "I'll trade up to N GB with
whoever redeems this." Redeeming it creates a **buddy pairing**, with the
same `pledged_bytes` on both sides.

### `POST /invites`
Auth: session + verified. Creates an invite, private by default.

| Body field | Required | Notes |
|---|---|---|
| `pledged_bytes` | yes | Multiple of 5 GB, 5 GB to 20,000 GB |
| `is_public` | no | `true` lists it in the marketplace |
| `note` | no | String shown on the listing |
| `country` | no | String, up to 100 characters, from the dashboard's country list (or "Other") |
| `speed_tier` | no | `under_10`, `10_50`, `50_200` or `200_plus` (Mbps) |
| `always_on` | no | Boolean |
| `relay_only` | no | Boolean |

`201 {"invite": {id, code, pledged_bytes, status, is_public, note,
country, speed_tier, always_on, relay_only, created_at}}`. `code` is 10
hex characters. `402` (with `requires_subscription: true`) if this would
take you past 50 GB pledged and you have no subscription and aren't
complimentary. This is only an early check; the binding one happens at
redeem time.

### `GET /invites`
Auth: session. Invites **you created**, newest first, including redeemed
ones (`status`, `redeemed_by_account_id`, `redeemed_at`).

### `POST /invites/:code/redeem`
Auth: session + verified. Redeems a private code. In one locked
transaction, it checks:
- the invite exists (`404`) and is still `pending` (`409 invite already redeemed`);
- you aren't redeeming your own invite (`400`);
- you aren't already actively paired with this account (`409`);
- the free allowance, for **each side separately**: an account without a
  subscription that isn't complimentary can't go past 50 GB (`402`; the
  redeemer's error has `requires_subscription: true`).

Then it creates the pairing, marks the invite redeemed, and updates both
sides' Stripe subscription quantity. `201 {"pairing": {id, account_a_id,
account_b_id, pledged_bytes, status, created_at}}`.

## Marketplace

### `GET /invites/public?pledged_bytes=<n>&country=&offset=&limit=`
Auth: session. Open public listings from **other** accounts, of exactly
one size. `pledged_bytes` is required, since pairs trade like-for-like.
`country` is optional and must match exactly; an empty value means any
country. `limit` is 1 to 100 (default 50), `offset` defaults to 0.

```json
{
  "listings": [{
    "id", "pledged_bytes", "note", "created_at", "country", "speed_tier",
    "always_on", "relay_only",
    "inviter_label", "inviter_member_since", "inviter_completed_pairings"
  }],
  "pledged_bytes", "total_count", "offset", "limit", "has_more"
}
```

`inviter_label` is the nickname, or `Buddy #<first 8 of account id>`.
Emails are never shown. `inviter_completed_pairings` counts active and
revoked pairings.

### `POST /invites/public/:id/redeem`
Auth: session + verified. Body `{pledged_bytes}`: the size the user saw.
It's the same as redeeming a code, plus `409` if the listing's real size
doesn't match. Same responses as `POST /invites/:code/redeem`.

## Billing

`routes/billing.js`. Storage is billed on **pledged** GB (a Stripe
subscription quantity that changes only when pairings change), tiered:
first 50 GB free, then $3/TB/month. Relay bandwidth is $0.02/GB, for paid
accounts only, as a second metered line item on the same subscription.

### `GET /billing/summary`
Auth: session.

| Field | Meaning |
|---|---|
| `pledged_gb` | GB pledged across active pairings (rounded up) |
| `free_allowance_gb` | 50 |
| `billable_gb` | `max(0, pledged_gb - 50)` |
| `estimated_monthly_usd` | An estimate only. Stripe is the source of truth |
| `has_subscription` | Account has a Stripe subscription |
| `billing_exempt` | Complimentary account: never needs a card |
| `relay_status` | `active` or `paused` |
| `relay_usd_per_gb` | 0.02 |
| `relay_billing_enabled` | `STRIPE_RELAY_PRICE_ID` is set on this deployment |
| `free_relay_gb_per_month` | Free accounts' monthly relay allowance |
| `relay_bytes_this_month` | Relay bytes recorded since the 1st (UTC) |

### `POST /billing/checkout`
Auth: session. Creates a Stripe Checkout session (subscription mode,
storage price, plus the relay price if configured) and returns
`{"url"}` to redirect to. It creates the Stripe customer if there isn't
one yet. `503` if `STRIPE_STORAGE_PRICE_ID` or `APP_URL` isn't set. `409`
if already subscribed. The subscription is linked to the account by the
`checkout.session.completed` webhook, not here.

### `POST /billing/portal`
Auth: session. Returns `{"url"}` for the Stripe Billing Portal (change
card, cancel, invoices). `400 no billing account yet` if there's no Stripe
customer.

## Support

### `POST /support`
Auth: session. `routes/support.js`. Emails the support mailbox
(`SUPPORT_EMAIL`) with the message and an account-details block, with
Reply-To set to the user. Nothing is stored.

| Body field | Required | Notes |
|---|---|---|
| `category` | yes | `account`, `billing`, `pairing`, `backup` or `other` |
| `message` | yes | 10 to 5,000 characters |
| `subject` | no | Up to 120 characters; newlines stripped |

`200 {"ok":true,"message"}`. `400` on a validation error, `429` at more
than 5 per hour, `502` if the SMTP send failed, `503` if SMTP isn't
configured.

## Relay access (relay-1 only)

### `POST /relay/access`
Auth: relay token. `routes/relay.js`. iroh-relay (`access.http` in
`deploy/relay/config.toml`) calls this on every device connection, with
the device's 64-hex node id in `X-Iroh-NodeId` (or
`X-Iroh-Endpoint-Id`). The response is **plain text**. The relay only
admits a `200` whose body is `true`:

| Result | Status | Body |
|---|---|---|
| A registered device on an account that isn't paused | 200 | `true` |
| Unknown node id, or a paused account | 403 | `false` |
| Bad or missing node id header | 400 | `false` |
| Wrong token | 401 | `false` |
| `RELAY_ACCESS_TOKEN` not set on the API | 503 | `false` |

Checked on connect only: a device that's already connected stays
connected until it reconnects.

## Stripe webhook

### `POST /webhooks/stripe`
Auth: Stripe signature. `routes/stripe-webhook.js`. Gets the raw body.
Responds `400` if the signature is bad, and `500` if handling fails (so
Stripe retries).

| Event | Effect |
|---|---|
| `checkout.session.completed` | Links the new subscription to the account (`client_reference_id`): stores the subscription id and its storage item id, and un-pauses relay |
| `invoice.payment_succeeded` | Un-pauses relay for that customer |
| `invoice.payment_failed` | Only once Stripe has **given up retrying** (`next_payment_attempt` is null): pauses relay with reason `payment_failed` and emails the user. Earlier failed attempts are ignored |
| `customer.subscription.deleted` | Clears the stored subscription and item ids |
| `customer.subscription.updated` | Only logged |
| anything else | Ignored, `200` |

## Admin

`routes/admin.js`, used by `/admin.html`. **Every route needs admin auth.**
`:id` is an account UUID. Actions marked *logged* print a line to the API
container's log: `[admin] <admin email> <action> account=<id> <details>`.
Read them on app-1 with
`docker compose logs api | grep '\[admin\]'` (from `deploy/app`). The logs
only reach back to the last time the container was recreated (every deploy).

### Read

| Route | Returns |
|---|---|
| `GET /admin/accounts` | Every account, newest first: `id, email, nickname, tier, relay_status, relay_paused_at, relay_paused_reason, created_at, billing_exempt, has_subscription, device_count, buddies[{email, pledged_bytes, status}], is_admin` |
| `GET /admin/stats` | Service totals: relay bytes (all time, this month, of which paid, accounts relaying), accounts (total, verified, subscribed, paused, new in 7 and 30 days), devices (total, online in the last 5 min, seen in 24 h, counts by client version), active pairings and pledged bytes, open marketplace listings. Cached for 12 hours; **`?refresh=1`** recomputes |
| `GET /admin/accounts/:id` | Everything for a support ticket: `account` (with Stripe ids, sign-in methods, verification, `billing_exempt`, `stale_warning_sent_at`), `devices` (with `client_version`), `pairings` (all statuses, with the other side's email), `invites` created, `redeemed_invites`, `activity` (active sessions, last sign-in), `pledge` (pledged, free, billable GB), `relay_usage` (total, last 30 days, this month, unsynced bytes and events, last event). Never includes password or token hashes. `404` if not found |
| `GET /admin/accounts/:id/stripe` | Live Stripe state: customer, subscriptions (items, quantities, periods, cancel state), payment method, recent invoices, plus `checks[]` (`level`: `info`, `warn`, `error`) comparing Stripe with the database (missing or deleted customer, unlinked subscription, several live subscriptions, quantity not matching pledged GB, no payment method, and so on). `{"configured": false}` if Stripe isn't set up |

### Account actions

| Route | Body | Does | Logged |
|---|---|---|---|
| `POST /admin/accounts/:id/password-reset` | none | Creates a single-use 24 h password link and emails it. Returns `{emailed, url, expires_in_hours}`; `url` is there to paste into a reply if email isn't configured. Works for Google-only accounts (it adds a password) | yes |
| `POST /admin/accounts/:id/verify-email` | none | Marks the email verified without the link | yes |
| `POST /admin/accounts/:id/resend-verification` | none | Resends the verification email. `409` if already verified, `429` within 60 s of the last one, `503` if SMTP isn't configured | yes |
| `POST /admin/accounts/:id/revoke-sessions` | none | Signs the account out of every browser. Device tokens aren't affected. `400` on your own account. Returns `{revoked}` | yes |
| `POST /admin/accounts/:id/relay` | `{action: "pause" \| "resume", reason?}` | **pause**: stops buddy syncing; `reason` (up to 100 characters, default `admin`) is shown in the admin list. **resume**: clears any pause, whatever the reason. A later failed payment, or the free cap on the next relay report, can pause it again | yes (`relay-pause` / `relay-resume`) |
| `POST /admin/accounts/:id/billing-exempt` | `{exempt: true \| false}` (must be a boolean) | Complimentary account. **true**: pledges past 50 GB need no subscription; relay is recorded but never billed or capped; a `free_relay_cap` pause is lifted (other pauses stay). **false**: back to normal; the free cap applies from the next relay report. Doesn't touch Stripe, so an existing subscription keeps billing storage until cancelled. Manual only: being an admin doesn't make an account exempt | yes |

### Stripe actions

`503` if `STRIPE_SECRET_KEY` isn't set.

| Route | Body | Does | Logged |
|---|---|---|---|
| `POST /admin/accounts/:id/stripe/relink` | none | Rebuilds the account's Stripe link from what Stripe has, to fix a missed webhook. Finds the customer (the stored id, else by email, preferring the one whose `metadata.account_id` matches; `409` if several and none match, `404` if none), picks the newest live subscription (`active`, `trialing`, `past_due`, `unpaid`), and stores the customer, subscription and storage item ids (clearing the subscription if there's none). `409` if those ids belong to another account. Returns the ids and `other_live_subscriptions` | yes |
| `POST /admin/accounts/:id/stripe/resync-quantity` | none | Sets the storage item quantity to the current pledged GB (at least 1), with proration. Unlike the automatic sync, it reports errors. `400` if no item is linked | yes |
| `POST /admin/accounts/:id/stripe/cancel` | `{mode: "period_end" \| "undo" \| "now"}` | **period_end**: cancel when the paid period ends. **undo**: reverse a pending period-end cancel. **now**: cancel immediately and clear the link. `400` if no subscription is linked | yes (`stripe-cancel-<mode>`) |

### Deletes

All are logged (`delete-device`, `delete-pairing`, `delete-invite`,
`delete-account`; the account line includes the deleted email, since the
row is gone afterwards). All return `204` (except the account delete) or
`404` if not found under that account.

| Route | Does |
|---|---|
| `DELETE /admin/accounts/:id/devices/:deviceId` | Removes one device of that account, which revokes its token |
| `DELETE /admin/accounts/:id/pairings/:pairingId` | Removes a pairing the account is part of (the row is deleted, not marked revoked) and updates **both** sides' Stripe quantities |
| `DELETE /admin/accounts/:id/invites/:inviteId` | Removes an invite the account created |
| `DELETE /admin/accounts/:id` | Deletes the account. Devices, pairings, invites it created, sessions and relay usage cascade; invites it redeemed keep the row with `redeemed_by_account_id` set to null. Updates each former buddy's Stripe quantity. **Doesn't cancel Stripe.** Returns `{ok, had_stripe_subscription, ended_active_pairings[{email, pledged_gb}]}`. `400` on your own account |

## Things there is no route for

Useful to know before promising them to a user. These are done by an
admin, or not at all:

- **Ending a pairing or withdrawing an invite yourself.** There's no user
  route; an admin deletes it from the account view.
- **Deleting your own account.** Admin only.
- **Changing your email.** Not supported.
- **Changing your password while signed in.** Use "Forgot password?"
  (`POST /auth/forgot-password`).
- **Granting admin.** Edit `ADMIN_EMAILS` in `deploy/app/.env` and restart
  the API.
