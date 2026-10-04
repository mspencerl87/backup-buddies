import { pool } from '../db/pool.js';
import { FREE_RELAY_CAP_REASON } from '../lib/relay-billing.js';

// Resumes free accounts that were paused for using up their monthly relay
// allowance (lib/relay-billing.js), once a new calendar month (UTC) has
// started. Runs hourly from index.js — idempotent, so a restart or a
// missed run just means it happens on the next one.
//
// Only touches pauses with reason FREE_RELAY_CAP_REASON; a payment-failed
// or admin pause is never cleared here. An account that subscribes before
// the 1st is resumed immediately by the checkout webhook instead.
export async function resetFreeRelayCaps() {
  const result = await pool.query(
    `UPDATE accounts
        SET relay_status = 'active',
            relay_paused_at = NULL,
            relay_paused_reason = NULL
      WHERE relay_status = 'paused'
        AND relay_paused_reason = $1
        AND relay_paused_at < date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
      RETURNING email`,
    [FREE_RELAY_CAP_REASON],
  );
  return result.rows.map((row) => row.email);
}
