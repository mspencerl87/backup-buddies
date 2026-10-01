import { Router } from 'express';
import crypto from 'node:crypto';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';

export const invitesRouter = Router();

// Short, URL-safe, hard to guess by brute force but easy to read aloud or
// paste into a text message — e.g. "7f3a9c2b1e".
function generateInviteCode() {
  return crypto.randomBytes(5).toString('hex');
}

// Create an invite: "I'll trade up to this much storage with whoever
// redeems this code." Nothing is paired yet — redeeming is a separate step
// by the other person.
invitesRouter.post('/invites', requireAuth, async (req, res) => {
  const { pledged_bytes } = req.body ?? {};

  const pledgedBytes = Number(pledged_bytes);
  if (!Number.isInteger(pledgedBytes) || pledgedBytes <= 0) {
    return res.status(400).json({ error: 'pledged_bytes must be a positive integer' });
  }

  const code = generateInviteCode();
  const result = await pool.query(
    `INSERT INTO invites (code, inviter_account_id, pledged_bytes)
     VALUES ($1, $2, $3)
     RETURNING id, code, pledged_bytes, status, created_at`,
    [code, req.account.id, pledgedBytes],
  );

  res.status(201).json({ invite: result.rows[0] });
});

// List invites *you created* — so you can find the code again to resend it,
// or see which ones have already been redeemed.
invitesRouter.get('/invites', requireAuth, async (req, res) => {
  const result = await pool.query(
    `SELECT id, code, pledged_bytes, status, created_at, redeemed_by_account_id, redeemed_at
       FROM invites
      WHERE inviter_account_id = $1
      ORDER BY created_at DESC`,
    [req.account.id],
  );
  res.json({ invites: result.rows });
});

// Redeem someone else's invite code: this is the moment a pairing actually
// becomes real. Runs as a transaction since it touches two tables and must
// not leave an invite marked redeemed without a matching pairing existing,
// or vice versa.
invitesRouter.post('/invites/:code/redeem', requireAuth, async (req, res) => {
  const { code } = req.params;
  const client = await pool.connect();

  try {
    await client.query('BEGIN');

    // Lock the invite row so two simultaneous redeem attempts on the same
    // code can't both succeed.
    const inviteResult = await client.query(
      `SELECT * FROM invites WHERE code = $1 FOR UPDATE`,
      [code],
    );
    const invite = inviteResult.rows[0];

    if (!invite) {
      await client.query('ROLLBACK');
      return res.status(404).json({ error: 'invite not found' });
    }
    if (invite.status !== 'pending') {
      await client.query('ROLLBACK');
      return res.status(409).json({ error: `invite already ${invite.status}` });
    }
    if (invite.inviter_account_id === req.account.id) {
      await client.query('ROLLBACK');
      return res.status(400).json({ error: "you can't redeem your own invite" });
    }

    const pairingResult = await client.query(
      `INSERT INTO buddy_pairings (account_a_id, account_b_id, pledged_bytes, status)
       VALUES ($1, $2, $3, 'active')
       RETURNING id, account_a_id, account_b_id, pledged_bytes, status, created_at`,
      [invite.inviter_account_id, req.account.id, invite.pledged_bytes],
    );

    await client.query(
      `UPDATE invites
          SET status = 'redeemed', redeemed_by_account_id = $1, redeemed_at = now()
        WHERE id = $2`,
      [req.account.id, invite.id],
    );

    await client.query('COMMIT');
    res.status(201).json({ pairing: pairingResult.rows[0] });
  } catch (err) {
    await client.query('ROLLBACK');
    throw err;
  } finally {
    client.release();
  }
});
