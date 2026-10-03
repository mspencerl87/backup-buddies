import { Router } from 'express';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';
import { isAdminEmail } from '../lib/admin.js';

export const meRouter = Router();

meRouter.get('/me', requireAuth, async (req, res) => {
  const [devices, pairings] = await Promise.all([
    pool.query(
      `SELECT id, iroh_node_id, label, token_last_four, created_at, last_seen_at
         FROM devices
        WHERE account_id = $1
        ORDER BY created_at`,
      [req.account.id],
    ),
    // Only 'active' -- a revoked pairing is dead weight on the dashboard
    // (nothing to do with it, no reactivate flow), so it's left off here
    // rather than piling up as stale rows forever. Not deleted though: the
    // marketplace's "completed pairings" trust signal on invites.js counts
    // revoked pairings too (see GET /invites/public), so the row still
    // needs to exist -- it's just not shown on the dashboard that created
    // it. The admin account-detail view (admin.js, not this endpoint) is
    // separate and still shows every status, since an admin investigating
    // an account needs the full history, not just what the account's own
    // dashboard considers current.
    pool.query(
      `SELECT id, account_a_id, account_b_id, pledged_bytes, status, created_at
         FROM buddy_pairings
        WHERE (account_a_id = $1 OR account_b_id = $1)
          AND status = 'active'
        ORDER BY created_at`,
      [req.account.id],
    ),
  ]);

  res.json({
    account: {
      id: req.account.id,
      email: req.account.email,
      nickname: req.account.nickname,
      // tier is set once at signup and nothing ever updates it afterward —
      // has_subscription (from stripe_subscription_id) is what actually
      // reflects whether this account is paying, same field billing.js's
      // /billing/summary already uses.
      tier: req.account.tier,
      has_subscription: Boolean(req.account.stripe_subscription_id),
      relay_status: req.account.relay_status,
      created_at: req.account.created_at,
      has_password: Boolean(req.account.password_hash),
      has_google: Boolean(req.account.google_sub),
      is_admin: isAdminEmail(req.account.email),
      email_verified: req.account.email_verified,
    },
    devices: devices.rows,
    pairings: pairings.rows,
  });
});

// Sets (or clears) the public display name shown on marketplace listings
// instead of your email address. Pass an empty string / null to clear it.
meRouter.patch('/me', requireAuth, async (req, res) => {
  const { nickname } = req.body ?? {};
  if (nickname !== undefined && nickname !== null && typeof nickname !== 'string') {
    return res.status(400).json({ error: 'nickname must be a string' });
  }
  if (typeof nickname === 'string' && nickname.length > 40) {
    return res.status(400).json({ error: 'nickname must be 40 characters or fewer' });
  }

  await pool.query('UPDATE accounts SET nickname = $1 WHERE id = $2', [
    nickname || null,
    req.account.id,
  ]);

  res.json({ ok: true });
});
