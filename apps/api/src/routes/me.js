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
    pool.query(
      `SELECT id, account_a_id, account_b_id, pledged_bytes, status, created_at
         FROM buddy_pairings
        WHERE account_a_id = $1 OR account_b_id = $1
        ORDER BY created_at`,
      [req.account.id],
    ),
  ]);

  res.json({
    account: {
      id: req.account.id,
      email: req.account.email,
      nickname: req.account.nickname,
      tier: req.account.tier,
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
