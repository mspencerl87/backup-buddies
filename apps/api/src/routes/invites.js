import { Router } from 'express';
import crypto from 'node:crypto';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';
import { syncSubscriptionQuantity } from './billing.js';

export const invitesRouter = Router();

// Short, URL-safe, hard to guess by brute force but easy to read aloud or
// paste into a text message — e.g. "7f3a9c2b1e".
function generateInviteCode() {
  return crypto.randomBytes(5).toString('hex');
}

// Create an invite: "I'll trade up to this much storage with whoever
// redeems this code." Nothing is paired yet — redeeming is a separate step
// by the other person. Optionally public: listed in the marketplace for
// anyone to find and request, instead of only usable by whoever you send
// the code to directly.
invitesRouter.post('/invites', requireAuth, async (req, res) => {
  const { pledged_bytes, is_public, note } = req.body ?? {};

  const pledgedBytes = Number(pledged_bytes);
  if (!Number.isInteger(pledgedBytes) || pledgedBytes <= 0) {
    return res.status(400).json({ error: 'pledged_bytes must be a positive integer' });
  }
  if (note !== undefined && note !== null && typeof note !== 'string') {
    return res.status(400).json({ error: 'note must be a string' });
  }

  const code = generateInviteCode();
  const result = await pool.query(
    `INSERT INTO invites (code, inviter_account_id, pledged_bytes, is_public, note)
     VALUES ($1, $2, $3, $4, $5)
     RETURNING id, code, pledged_bytes, status, is_public, note, created_at`,
    [code, req.account.id, pledgedBytes, Boolean(is_public), note || null],
  );

  res.status(201).json({ invite: result.rows[0] });
});

// List invites *you created* — so you can find the code again to resend it,
// or see which ones have already been redeemed.
invitesRouter.get('/invites', requireAuth, async (req, res) => {
  const result = await pool.query(
    `SELECT id, code, pledged_bytes, status, is_public, note, created_at,
            redeemed_by_account_id, redeemed_at
       FROM invites
      WHERE inviter_account_id = $1
      ORDER BY created_at DESC`,
    [req.account.id],
  );
  res.json({ invites: result.rows });
});

// Shared by both redeem routes below (private code, or public marketplace
// listing): locks the invite row, validates it, creates the resulting
// buddy_pairings row, and marks the invite redeemed — all in one
// transaction, so an invite can never end up marked redeemed without a
// matching pairing existing, or vice versa.
async function redeemInvite({ lockQuery, lockParam, redeemerAccountId }) {
  const client = await pool.connect();
  try {
    await client.query('BEGIN');

    const inviteResult = await client.query(lockQuery, [lockParam]);
    const invite = inviteResult.rows[0];

    if (!invite) {
      await client.query('ROLLBACK');
      return { status: 404, body: { error: 'invite not found' } };
    }
    if (invite.status !== 'pending') {
      await client.query('ROLLBACK');
      return { status: 409, body: { error: `invite already ${invite.status}` } };
    }
    if (invite.inviter_account_id === redeemerAccountId) {
      await client.query('ROLLBACK');
      return { status: 400, body: { error: "you can't redeem your own invite" } };
    }

    const pairingResult = await client.query(
      `INSERT INTO buddy_pairings (account_a_id, account_b_id, pledged_bytes, status)
       VALUES ($1, $2, $3, 'active')
       RETURNING id, account_a_id, account_b_id, pledged_bytes, status, created_at`,
      [invite.inviter_account_id, redeemerAccountId, invite.pledged_bytes],
    );

    await client.query(
      `UPDATE invites
          SET status = 'redeemed', redeemed_by_account_id = $1, redeemed_at = now()
        WHERE id = $2`,
      [redeemerAccountId, invite.id],
    );

    await client.query('COMMIT');

    // Pairing is now active — bring each side's Stripe subscription
    // quantity (if they have one) up to date with their new total pledge.
    // Deliberately after the commit: a Stripe hiccup here should never
    // roll back a successful pairing, and syncSubscriptionQuantity never
    // throws on its own.
    await syncSubscriptionQuantity(invite.inviter_account_id);
    await syncSubscriptionQuantity(redeemerAccountId);

    return { status: 201, body: { pairing: pairingResult.rows[0] } };
  } catch (err) {
    await client.query('ROLLBACK');
    throw err;
  } finally {
    client.release();
  }
}

// Redeem someone else's invite code directly (they sent it to you).
invitesRouter.post('/invites/:code/redeem', requireAuth, async (req, res) => {
  const result = await redeemInvite({
    lockQuery: `SELECT * FROM invites WHERE code = $1 FOR UPDATE`,
    lockParam: req.params.code,
    redeemerAccountId: req.account.id,
  });
  res.status(result.status).json(result.body);
});

// ---- Marketplace: for invites marked public, findable without a code ----

// Browse open public listings from *other* accounts. Shows a nickname
// (falling back to a short, non-reversible label) instead of the
// inviter's email — nothing here reveals who someone is unless they've
// chosen to set a nickname.
invitesRouter.get('/invites/public', requireAuth, async (req, res) => {
  const result = await pool.query(
    `SELECT invites.id, invites.pledged_bytes, invites.note, invites.created_at,
            COALESCE(accounts.nickname, 'Buddy #' || substring(accounts.id::text, 1, 8)) AS inviter_label
       FROM invites
       JOIN accounts ON accounts.id = invites.inviter_account_id
      WHERE invites.is_public = true
        AND invites.status = 'pending'
        AND invites.inviter_account_id <> $1
      ORDER BY invites.created_at DESC`,
    [req.account.id],
  );
  res.json({ listings: result.rows });
});

// Request a pairing from a public listing — same effect as redeeming a
// code, just looked up by listing id instead of needing the code texted
// to you directly.
invitesRouter.post('/invites/public/:id/redeem', requireAuth, async (req, res) => {
  const result = await redeemInvite({
    lockQuery: `SELECT * FROM invites WHERE id = $1 AND is_public = true FOR UPDATE`,
    lockParam: req.params.id,
    redeemerAccountId: req.account.id,
  });
  res.status(result.status).json(result.body);
});
