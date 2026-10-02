import { Router } from 'express';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';
import { requireAdmin } from '../middleware/require-admin.js';

export const adminRouter = Router();

// Every route here is both session-authed AND restricted to ADMIN_EMAILS
// (see lib/admin.js) — requireAuth first so req.account exists for
// requireAdmin to check, and so an unauthenticated caller gets a plain 401
// rather than a 403 that would confirm admin routes exist at all.
adminRouter.use(requireAuth, requireAdmin);

// All accounts, with cheap counts and a buddy summary for a quick scan —
// not the full detail (devices/pairings/invites), which is a separate
// per-account fetch below so this list stays fast regardless of how many
// accounts exist. The buddies array only carries pledged_bytes (what's
// agreed over the API) — there's no "used" figure to show here, since
// actual usage is only ever known locally on each client's own dashboard
// and isn't reported back to this server.
adminRouter.get('/admin/accounts', async (req, res) => {
  const result = await pool.query(
    `SELECT accounts.id, accounts.email, accounts.nickname, accounts.tier,
            accounts.relay_status, accounts.created_at,
            (accounts.stripe_subscription_id IS NOT NULL) AS has_subscription,
            (SELECT count(*) FROM devices
              WHERE devices.account_id = accounts.id) AS device_count,
            COALESCE(buddies.buddies, '[]'::json) AS buddies
       FROM accounts
       LEFT JOIN LATERAL (
         SELECT json_agg(
                  json_build_object(
                    'email', other.email,
                    'pledged_bytes', buddy_pairings.pledged_bytes,
                    'status', buddy_pairings.status
                  ) ORDER BY other.email
                ) AS buddies
           FROM buddy_pairings
           JOIN accounts other
             ON other.id = CASE WHEN buddy_pairings.account_a_id = accounts.id
                                 THEN buddy_pairings.account_b_id
                                 ELSE buddy_pairings.account_a_id END
          WHERE buddy_pairings.account_a_id = accounts.id
             OR buddy_pairings.account_b_id = accounts.id
       ) buddies ON true
      ORDER BY accounts.created_at DESC`,
  );
  res.json({ accounts: result.rows });
});

// One account's full detail — everything you'd need on a support ticket:
// its devices (to spot a stuck/misregistered one), its buddy pairings
// (with the other side's email, not just an id), and the invites it has
// created. Deliberately leaves out password_hash and token_hash — nothing
// here needs them, and there's no reason to ever pull them over the wire.
adminRouter.get('/admin/accounts/:id', async (req, res) => {
  const accountId = req.params.id;

  const [accountResult, devicesResult, pairingsResult, invitesResult] = await Promise.all([
    pool.query(
      `SELECT id, email, nickname, tier, relay_status, created_at,
              stripe_customer_id, stripe_subscription_id,
              (password_hash <> '') AS has_password,
              (google_sub IS NOT NULL) AS has_google,
              email_verified
         FROM accounts
        WHERE id = $1`,
      [accountId],
    ),
    pool.query(
      `SELECT id, iroh_node_id, label, token_last_four, created_at, last_seen_at
         FROM devices
        WHERE account_id = $1
        ORDER BY created_at`,
      [accountId],
    ),
    pool.query(
      `SELECT buddy_pairings.id, buddy_pairings.account_a_id, buddy_pairings.account_b_id,
              buddy_pairings.pledged_bytes, buddy_pairings.status, buddy_pairings.created_at,
              other.email AS other_email
         FROM buddy_pairings
         JOIN accounts other
           ON other.id = CASE WHEN buddy_pairings.account_a_id = $1
                               THEN buddy_pairings.account_b_id
                               ELSE buddy_pairings.account_a_id END
        WHERE buddy_pairings.account_a_id = $1 OR buddy_pairings.account_b_id = $1
        ORDER BY buddy_pairings.created_at DESC`,
      [accountId],
    ),
    pool.query(
      `SELECT id, code, pledged_bytes, status, is_public, note, created_at,
              redeemed_by_account_id, redeemed_at
         FROM invites
        WHERE inviter_account_id = $1
        ORDER BY created_at DESC`,
      [accountId],
    ),
  ]);

  if (accountResult.rows.length === 0) {
    return res.status(404).json({ error: 'account not found' });
  }

  res.json({
    account: accountResult.rows[0],
    devices: devicesResult.rows,
    pairings: pairingsResult.rows,
    invites: invitesResult.rows,
  });
});

// Remove one device, scoped to the account it's under so a copy-pasted
// device id can't accidentally reach across accounts.
adminRouter.delete('/admin/accounts/:id/devices/:deviceId', async (req, res) => {
  const result = await pool.query(
    `DELETE FROM devices WHERE id = $1 AND account_id = $2 RETURNING id`,
    [req.params.deviceId, req.params.id],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'device not found' });
  }
  res.status(204).end();
});

// Remove a buddy pairing — scoped to pairings this account is actually
// part of (as either side), same reasoning as the device route above.
adminRouter.delete('/admin/accounts/:id/pairings/:pairingId', async (req, res) => {
  const result = await pool.query(
    `DELETE FROM buddy_pairings
      WHERE id = $1 AND (account_a_id = $2 OR account_b_id = $2)
      RETURNING id`,
    [req.params.pairingId, req.params.id],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'pairing not found' });
  }
  res.status(204).end();
});

// Remove an invite the account created (not one it redeemed from someone
// else — that's the inviter's to remove).
adminRouter.delete('/admin/accounts/:id/invites/:inviteId', async (req, res) => {
  const result = await pool.query(
    `DELETE FROM invites WHERE id = $1 AND inviter_account_id = $2 RETURNING id`,
    [req.params.inviteId, req.params.id],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'invite not found' });
  }
  res.status(204).end();
});

// Delete the whole account. devices/buddy_pairings/invites/sessions/
// relay_usage_events all reference accounts with ON DELETE CASCADE (see
// db/init.sql and migrations), so this one statement cleans up everything
// in Postgres. It does NOT touch Stripe — a subscription there keeps
// running and billing until it's cancelled separately in the Stripe
// dashboard (or via its API). had_stripe_subscription in the response
// flags when that manual step is needed; the dashboard UI warns on it too.
adminRouter.delete('/admin/accounts/:id', async (req, res) => {
  if (req.params.id === req.account.id) {
    return res.status(400).json({ error: "can't delete your own account from here" });
  }

  const result = await pool.query(
    `DELETE FROM accounts WHERE id = $1 RETURNING id, stripe_subscription_id`,
    [req.params.id],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'account not found' });
  }

  res.json({
    ok: true,
    had_stripe_subscription: Boolean(result.rows[0].stripe_subscription_id),
  });
});
