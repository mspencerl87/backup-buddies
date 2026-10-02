import { pool } from '../db/pool.js';
import { stripe } from '../lib/stripe.js';
import { RELAY_METER_EVENT_NAME } from '../lib/relay-billing.js';

const BYTES_PER_MB = 1024 * 1024;

// Aggregates unsynced relay_usage_events per account and reports the total
// to Stripe as a Billing Meter event, then marks those rows synced. Meant
// to be called on a schedule (see index.js) rather than per-request — Stripe
// meter events don't need to be real-time, and batching keeps this from
// hammering Stripe's API every time a client polls.
//
// Rounds down to whole megabytes per account per run; any leftover
// fractional MB stays unsynced and simply rolls into the next run's sum, so
// nothing is ever lost or double-counted — each row is summed and marked
// synced exactly once, only once its batch's total actually clears 1MB.
export async function syncRelayUsageToStripe() {
  const result = await pool.query(
    `SELECT relay_usage_events.account_id,
            accounts.stripe_customer_id,
            SUM(relay_usage_events.bytes_relayed) AS total_bytes,
            array_agg(relay_usage_events.id) AS event_ids
       FROM relay_usage_events
       JOIN accounts ON accounts.id = relay_usage_events.account_id
      WHERE relay_usage_events.synced_to_stripe = false
        AND accounts.stripe_customer_id IS NOT NULL
      GROUP BY relay_usage_events.account_id, accounts.stripe_customer_id`,
  );

  let reported = 0;
  for (const row of result.rows) {
    const mb = Math.floor(Number(row.total_bytes) / BYTES_PER_MB);
    if (mb <= 0) continue; // not enough accumulated yet — leave unsynced for next run

    try {
      await stripe.billing.meterEvents.create({
        event_name: RELAY_METER_EVENT_NAME,
        payload: { stripe_customer_id: row.stripe_customer_id, value: String(mb) },
      });
      await pool.query(
        `UPDATE relay_usage_events SET synced_to_stripe = true WHERE id = ANY($1::uuid[])`,
        [row.event_ids],
      );
      reported += 1;
    } catch (err) {
      // Leave unsynced — next run retries. Most likely cause: this account's
      // subscription doesn't carry the relay price yet (see
      // scripts/add-relay-price-to-existing-subscriptions.mjs) — not data
      // loss, just not billable until that's fixed.
      console.error(`Failed to report relay usage to Stripe for account ${row.account_id}:`, err);
    }
  }
  return reported;
}
