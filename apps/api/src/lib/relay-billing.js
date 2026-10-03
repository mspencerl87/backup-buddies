import { pool } from '../db/pool.js';

// Shared relay-bandwidth billing constants and the write side of the
// pipeline (the read/report side lives in jobs/sync-relay-usage.js).
//
// Relay billing is paid-tier only: free accounts (under the 50GB pledge
// allowance, see billing.js's FREE_GB) are never charged for relay
// bandwidth, no matter how much of it they use.

// Must match the event_name used when creating the Stripe Billing Meter —
// see scripts/create-stripe-relay-meter-and-price.mjs. Stripe's meter-events
// API is addressed by this name, not the meter's id (STRIPE_RELAY_METER_ID
// in .env is informational only — nothing in this codebase reads it).
export const RELAY_METER_EVENT_NAME = 'relay_bandwidth_mb';

// $0.02/GB. Linode (who host relay.filegarden.net) charges roughly
// $0.005-0.01/GB for network egress overage as of 2026, so this keeps a
// healthy margin over the actual cost of relayed traffic while staying far
// below typical cloud egress pricing and easy to explain to users. See
// scripts/create-stripe-relay-meter-and-price.mjs for the Price this
// becomes in Stripe (reported in whole megabytes, not bytes or fractional
// GB, so the per-unit amount doesn't need a many-decimal-place value).
export const RELAY_USD_PER_GB = 0.02;

// A single /devices/me/buddies report claiming more than this many relayed
// megabytes (100GB) in one ~30s poll cycle is almost certainly a bug, not
// real traffic — clamp rather than trust it outright, so a malformed or
// misbehaving client can't balloon someone's bill from one bad report.
export const MAX_RELAY_MB_PER_REPORT = 100_000;

// Records relay megabytes a device reported since its last successful
// report. Silently discarded (not just unsynced — never written) for an
// account with no active Stripe subscription, so relay_usage_events never
// accumulates rows for free-tier traffic that could never be billed anyway.
// Safe to call on every poll — it's one cheap lookup plus one insert, only
// when the client actually reports nonzero relay bytes.
//
// Also discarded while relay_status isn't 'active' (a paused account —
// e.g. a failed payment, see stripe-webhook.js) — the client is expected
// to stop syncing entirely once it sees `paused` on this same endpoint
// (devices.js), but that's enforced client-side, and a stale or
// not-yet-updated client could still report usage in the meantime. This
// is the actual backstop that guarantees a paused account can't accrue
// more relay fees no matter what the client does.
export async function recordRelayUsage(accountId, relayMb) {
  if (!Number.isInteger(relayMb) || relayMb <= 0) return;
  const clampedMb = Math.min(relayMb, MAX_RELAY_MB_PER_REPORT);

  const account = await pool.query(
    `SELECT stripe_subscription_id, relay_status FROM accounts WHERE id = $1`,
    [accountId],
  );
  if (!account.rows[0]?.stripe_subscription_id) return;
  if (account.rows[0].relay_status !== 'active') return;

  const bytes = BigInt(clampedMb) * 1024n * 1024n;
  await pool.query(
    `INSERT INTO relay_usage_events (account_id, bytes_relayed, occurred_at)
     VALUES ($1, $2, now())`,
    [accountId, bytes.toString()],
  );
}
