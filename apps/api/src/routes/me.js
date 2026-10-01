import { Router } from 'express';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';

export const meRouter = Router();

meRouter.get('/me', requireAuth, async (req, res) => {
  const [devices, pairings] = await Promise.all([
    pool.query(
      `SELECT id, iroh_node_id, label, created_at, last_seen_at
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
      tier: req.account.tier,
      relay_status: req.account.relay_status,
      created_at: req.account.created_at,
    },
    devices: devices.rows,
    pairings: pairings.rows,
  });
});
