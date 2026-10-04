import { pool } from '../db/pool.js';
import { sendMail } from './mailer.js';

// Shared relay-bandwidth billing constants and the write side of the
// pipeline (the read/report side lives in jobs/sync-relay-usage.js).
//
// Relay billing is paid-tier only: free accounts (under the 50GB pledge
// allowance, see billing.js's FREE_GB) are never charged for relay
// bandwidth. They do get a monthly allowance instead (FREE_RELAY_GB below):
// their usage is recorded but never sent to Stripe, and crossing the
// allowance pauses the account's buddy syncing until the 1st of next month
// (jobs/reset-free-relay-cap.js) or until it subscribes.

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

// Monthly relay allowance for free (unsubscribed) accounts, in GB. Only
// traffic that actually goes through relay-1 counts — direct and LAN
// connections are free and unlimited. Overridable per deployment.
export const FREE_RELAY_GB_PER_MONTH = Number(process.env.FREE_RELAY_GB_PER_MONTH || 10);
const BYTES_PER_GB = 1024 ** 3;

// relay_paused_reason for an account paused by the free allowance — what
// jobs/reset-free-relay-cap.js looks for to resume it on the 1st.
export const FREE_RELAY_CAP_REASON = 'free_relay_cap';

// Relay bytes a free account has used since the start of this calendar
// month (UTC). Paid accounts' rows are summed too if asked, but nothing
// caps them.
export async function relayBytesThisMonth(accountId) {
  const result = await pool.query(
    `SELECT COALESCE(SUM(bytes_relayed), 0) AS bytes
       FROM relay_usage_events
      WHERE account_id = $1
        AND occurred_at >= date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'`,
    [accountId],
  );
  return Number(result.rows[0].bytes);
}

// Records relay megabytes a device reported since its last successful
// report. Safe to call on every poll — a couple of cheap queries, only
// when the client actually reports nonzero relay bytes.
//
// Discarded entirely while relay_status isn't 'active' (a paused account —
// a failed payment, see stripe-webhook.js, or the free allowance below).
// The client is expected to stop syncing once it sees `paused` on this
// same endpoint (devices.js), but that's enforced client-side, and a stale
// or not-yet-updated client could still report usage in the meantime.
//
// Subscribed accounts: written unsynced, for jobs/sync-relay-usage.js to
// bill. Free accounts: written as already "synced" so they're never sent
// to Stripe (and never billed retroactively if the account subscribes
// later) — they're recorded only to enforce FREE_RELAY_GB_PER_MONTH and to
// show usage on the dashboard and admin page.
export async function recordRelayUsage(accountId, relayMb) {
  if (!Number.isInteger(relayMb) || relayMb <= 0) return;
  const clampedMb = Math.min(relayMb, MAX_RELAY_MB_PER_REPORT);

  const account = await pool.query(
    `SELECT email, stripe_subscription_id, relay_status FROM accounts WHERE id = $1`,
    [accountId],
  );
  const row = account.rows[0];
  if (!row || row.relay_status !== 'active') return;

  const isFree = !row.stripe_subscription_id;
  const bytes = BigInt(clampedMb) * 1024n * 1024n;
  await pool.query(
    `INSERT INTO relay_usage_events (account_id, bytes_relayed, occurred_at, synced_to_stripe)
     VALUES ($1, $2, now(), $3)`,
    [accountId, bytes.toString(), isFree],
  );

  if (isFree) await enforceFreeRelayCap(accountId, row.email);
}

// Pauses a free account once this month's relay usage passes the
// allowance, and tells them why. The UPDATE only matches an account that
// is still active and still unsubscribed, so a duplicate call (or one that
// races a just-completed checkout) is a no-op and never re-sends the email.
async function enforceFreeRelayCap(accountId, email) {
  const usedBytes = await relayBytesThisMonth(accountId);
  if (usedBytes < FREE_RELAY_GB_PER_MONTH * BYTES_PER_GB) return;

  const result = await pool.query(
    `UPDATE accounts
        SET relay_status = 'paused',
            relay_paused_at = now(),
            relay_paused_reason = $2
      WHERE id = $1 AND relay_status = 'active' AND stripe_subscription_id IS NULL
      RETURNING id`,
    [accountId, FREE_RELAY_CAP_REASON],
  );
  if (result.rows.length === 0) return;

  console.log(`Paused free account ${email}: used its ${FREE_RELAY_GB_PER_MONTH} GB monthly relay allowance.`);
  const appUrl = process.env.APP_URL || '';
  try {
    await sendMail({
      to: email,
      subject: `Backup Buddies: you've used this month's ${FREE_RELAY_GB_PER_MONTH} GB of free relay bandwidth`,
      text:
        `Your devices couldn't connect to your buddy directly, so your backups have been going through our relay — ` +
        `and you've now used this month's free ${FREE_RELAY_GB_PER_MONTH} GB of it. Buddy syncing is paused until the ` +
        `1st of next month. Your existing backups are untouched.\n\n` +
        `To keep syncing now, add a payment method — relay traffic on a paid plan is $${RELAY_USD_PER_GB.toFixed(2)}/GB, ` +
        `and syncing resumes right away: ${appUrl}/dashboard.html#billing\n\n` +
        `Tip: if your device and your buddy's can reach each other directly (e.g. by forwarding a port on your router), ` +
        `that traffic doesn't use the relay at all and is always free.`,
      html:
        `<p>Your devices couldn't connect to your buddy directly, so your backups have been going through our relay — ` +
        `and you've now used this month's free ${FREE_RELAY_GB_PER_MONTH} GB of it. Buddy syncing is paused until the ` +
        `1st of next month. Your existing backups are untouched.</p>` +
        `<p>To keep syncing now, <a href="${appUrl}/dashboard.html#billing">add a payment method</a> — relay traffic on a ` +
        `paid plan is $${RELAY_USD_PER_GB.toFixed(2)}/GB, and syncing resumes right away.</p>` +
        `<p>Tip: if your device and your buddy's can reach each other directly (e.g. by forwarding a port on your router), ` +
        `that traffic doesn't use the relay at all and is always free.</p>`,
    });
  } catch (err) {
    console.error(`Failed to send free-relay-cap email to ${email}:`, err);
  }
}
