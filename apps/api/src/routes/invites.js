import { Router } from 'express';
import crypto from 'node:crypto';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';
import { requireEmailVerified } from '../middleware/require-email-verified.js';
import { rateLimit } from '../lib/rate-limit.js';
import { syncSubscriptionQuantity, pledgedGBForAccount, FREE_GB } from './billing.js';

export const invitesRouter = Router();

// Invite codes are 10 hex characters — fine against casual guessing, but
// nothing stopped a logged-in account from trying codes in a loop to land
// in a stranger's pairing. Per account (not IP), so it can't be dodged by
// faking a header; generous enough that a real person never sees it.
const redeemPerAccount = rateLimit({
  name: 'redeem-account',
  windowMs: 60 * 60 * 1000,
  max: 20,
  key: (req) => req.account?.id,
  message: 'Too many invite attempts — try again in an hour.',
});

// Pledges are constrained to clean, even steps rather than an arbitrary
// byte count — easier to reason about across pairings/billing, and avoids
// someone fat-fingering "5000000" instead of "50". 5GB steps, 5GB to
// 20,000GB (20TB) — the same GB basis (1024**3 bytes) the dashboard already
// displays everywhere, just capped and quantized.
//
// The step used to be 500GB, which silently broke the free tier: FREE_GB
// below is 50, and every pledge has to be at least one step, so nobody on
// the free tier could ever create a pairing at all — the smallest
// possible pledge was already 10x over their entire free allowance,
// forcing "subscribe first" on literally the first thing a new account
// tried to do. 5GB steps mean the free allowance actually fits ten of
// them, so trying the product without a card attached works the way it's
// meant to.
const GB = 1024 ** 3;
const PLEDGE_STEP_BYTES = 5 * GB;
const PLEDGE_MAX_BYTES = 20_000 * GB;

// Self-reported listing criteria (country, speed_tier, always_on,
// relay_only — see migration 010) — displayed on marketplace listings so
// a browser can judge fit for themselves (unlike pledged_bytes, these are
// never used to filter what the API returns; see GET /invites/public).
// Only speed_tier needs a fixed set to validate against; country is
// free-text-ish (checked against the dashboard's own list client-side,
// not re-validated here, since "Other" is a valid choice) and the two
// booleans just coerce.
const SPEED_TIERS = ['under_10', '10_50', '50_200', '200_plus'];
const MAX_COUNTRY_LENGTH = 100;

// Default page size for GET /invites/public, and the most a caller can
// ask for — a marketplace could grow into hundreds of listings at one
// size, and this is meant to be paged through (see find-a-buddy.html),
// never loaded all at once.
const MARKETPLACE_PAGE_SIZE = 50;
const MARKETPLACE_MAX_PAGE_SIZE = 100;

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
invitesRouter.post('/invites', requireAuth, requireEmailVerified, async (req, res) => {
  const { pledged_bytes, is_public, note, country, speed_tier, always_on, relay_only } =
    req.body ?? {};

  const pledgedBytes = Number(pledged_bytes);
  if (
    !Number.isInteger(pledgedBytes) ||
    pledgedBytes < PLEDGE_STEP_BYTES ||
    pledgedBytes > PLEDGE_MAX_BYTES ||
    pledgedBytes % PLEDGE_STEP_BYTES !== 0
  ) {
    return res.status(400).json({
      error: `pledged_bytes must be a multiple of ${PLEDGE_STEP_BYTES / GB}GB, between ${PLEDGE_STEP_BYTES / GB}GB and ${PLEDGE_MAX_BYTES / GB}GB`,
    });
  }
  if (note !== undefined && note !== null && typeof note !== 'string') {
    return res.status(400).json({ error: 'note must be a string' });
  }
  if (
    country !== undefined &&
    country !== null &&
    country !== '' &&
    (typeof country !== 'string' || country.length > MAX_COUNTRY_LENGTH)
  ) {
    return res.status(400).json({ error: `country must be a string up to ${MAX_COUNTRY_LENGTH} characters` });
  }
  if (speed_tier !== undefined && speed_tier !== null && speed_tier !== '' && !SPEED_TIERS.includes(speed_tier)) {
    return res.status(400).json({ error: `speed_tier must be one of: ${SPEED_TIERS.join(', ')}` });
  }

  // First line of defense against pledging past the free allowance with no
  // way to pay for it: this doesn't commit anything by itself (an invite
  // only becomes a real pledge once someone redeems it — see the same
  // check, done for real, in redeemInvite below), but there's no reason to
  // let someone generate a code for storage they can't legally accept.
  // Complimentary (billing_exempt, set by an admin) accounts never need a
  // card, so they skip this.
  const newPledgeGB = pledgedBytes / GB;
  const currentPledgeGB = await pledgedGBForAccount(req.account.id);
  if (
    currentPledgeGB + newPledgeGB > FREE_GB &&
    !req.account.stripe_subscription_id &&
    !req.account.billing_exempt
  ) {
    return res.status(402).json({
      error: `Creating this invite would bring your pledged total to ${currentPledgeGB + newPledgeGB}GB, over the ${FREE_GB}GB free allowance. Subscribe first (Billing, on your dashboard), then create the invite.`,
      requires_subscription: true,
    });
  }

  const code = generateInviteCode();
  const result = await pool.query(
    `INSERT INTO invites
       (code, inviter_account_id, pledged_bytes, is_public, note,
        country, speed_tier, always_on, relay_only)
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
     RETURNING id, code, pledged_bytes, status, is_public, note, country,
               speed_tier, always_on, relay_only, created_at`,
    [
      code,
      req.account.id,
      pledgedBytes,
      Boolean(is_public),
      note || null,
      country || null,
      speed_tier || null,
      Boolean(always_on),
      Boolean(relay_only),
    ],
  );

  res.status(201).json({ invite: result.rows[0] });
});

// List invites *you created* — so you can find the code again to resend it,
// or see which ones have already been redeemed.
invitesRouter.get('/invites', requireAuth, async (req, res) => {
  const result = await pool.query(
    `SELECT id, code, pledged_bytes, status, is_public, note, country,
            speed_tier, always_on, relay_only, created_at,
            redeemed_by_account_id, redeemed_at
       FROM invites
      WHERE inviter_account_id = $1
      ORDER BY created_at DESC`,
    [req.account.id],
  );
  res.json({ invites: result.rows });
});

// Same query as billing.js's pledgedGBForAccount, but run on the
// transaction's own client so it sees a consistent snapshot under the
// invite row's lock — two people redeeming invites from/to the same
// account at the same instant should never both be let through past the
// free allowance just because neither saw the other's pairing yet.
async function pledgedGBForAccountTx(client, accountId) {
  const result = await client.query(
    `SELECT COALESCE(SUM(pledged_bytes), 0) AS total_bytes
       FROM buddy_pairings
      WHERE (account_a_id = $1 OR account_b_id = $1)
        AND status = 'active'`,
    [accountId],
  );
  return Math.ceil(Number(result.rows[0].total_bytes) / GB);
}

// Shared by both redeem routes below (private code, or public marketplace
// listing): locks the invite row, validates it, creates the resulting
// buddy_pairings row, and marks the invite redeemed — all in one
// transaction, so an invite can never end up marked redeemed without a
// matching pairing existing, or vice versa.
async function redeemInvite({ lockQuery, lockParam, redeemerAccountId, expectedPledgedBytes }) {
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

    // Catches the common case (redeeming a second invite between two
    // accounts that already paired up, e.g. both sides sent each other a
    // code and redeemed both) with a clear error instead of a bare 500.
    // Not a race-safe guarantee by itself -- two redemptions between the
    // same pair committing at the same instant could both pass this
    // SELECT -- idx_pairings_unique_active_pair (migration 011) is what
    // actually makes it impossible; this is just the friendlier front
    // door for the ordinary, non-concurrent case.
    const existingPairing = await client.query(
      `SELECT id FROM buddy_pairings
        WHERE status = 'active'
          AND ((account_a_id = $1 AND account_b_id = $2)
            OR (account_a_id = $2 AND account_b_id = $1))
        LIMIT 1`,
      [invite.inviter_account_id, redeemerAccountId],
    );
    if (existingPairing.rows[0]) {
      await client.query('ROLLBACK');
      return {
        status: 409,
        body: { error: 'you and this account are already paired up' },
      };
    }

    // Marketplace listings are matched by exact pledge size — see
    // "Find a buddy" in dashboard.html, which only ever shows the redeemer
    // one size tier at a time and sends that size back here. This re-checks
    // it server-side (inside the same lock as everything else) so a stale
    // page — the listing's size can never actually change post-creation,
    // but a *different* listing at the id could in principle, or the UI
    // filter could simply be out of date — can't slip someone into a
    // pairing at a size they never saw or agreed to.
    if (
      expectedPledgedBytes !== undefined &&
      Number(invite.pledged_bytes) !== expectedPledgedBytes
    ) {
      await client.query('ROLLBACK');
      return {
        status: 409,
        body: {
          error: `this listing pledges ${Math.ceil(Number(invite.pledged_bytes) / GB)}GB, not ${Math.ceil(expectedPledgedBytes / GB)}GB — refresh "Find a buddy" and try again`,
        },
      };
    }

    // The real enforcement of the free allowance: creating this pairing is
    // the moment the pledge actually becomes binding on both sides (see
    // pledgedGBForAccount in billing.js — it only counts *active*
    // pairings, not invites). Checked here, inside the same transaction
    // and lock as the insert below, rather than relying on the
    // best-effort check in POST /invites, which can't see pairings
    // created since the invite was issued or a subscription that's since
    // lapsed. A complimentary (billing_exempt) account counts as able to
    // pay — each side is checked on its own, so pairing a comp account
    // with a free one still holds the free side to its allowance.
    const newPledgeGB = Math.ceil(Number(invite.pledged_bytes) / GB);
    const inviterCurrentGB = await pledgedGBForAccountTx(client, invite.inviter_account_id);
    const redeemerCurrentGB = await pledgedGBForAccountTx(client, redeemerAccountId);
    const subsResult = await client.query(
      `SELECT id, stripe_subscription_id, billing_exempt FROM accounts WHERE id = ANY($1::uuid[])`,
      [[invite.inviter_account_id, redeemerAccountId]],
    );
    const canPayPastFree = new Map(
      subsResult.rows.map((row) => [row.id, Boolean(row.stripe_subscription_id) || row.billing_exempt]),
    );

    if (
      inviterCurrentGB + newPledgeGB > FREE_GB &&
      !canPayPastFree.get(invite.inviter_account_id)
    ) {
      await client.query('ROLLBACK');
      return {
        status: 402,
        body: {
          error:
            "This pairing can't be accepted — it would put the invite creator over their 50GB free allowance, and they don't have an active subscription.",
        },
      };
    }
    if (redeemerCurrentGB + newPledgeGB > FREE_GB && !canPayPastFree.get(redeemerAccountId)) {
      await client.query('ROLLBACK');
      return {
        status: 402,
        body: {
          error: `Accepting this would bring your pledged total to ${redeemerCurrentGB + newPledgeGB}GB, over the ${FREE_GB}GB free allowance. Subscribe first (Billing, on your dashboard), then try again.`,
          requires_subscription: true,
        },
      };
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
    // The race the SELECT above can't catch: two redemptions between the
    // same pair of accounts committing at the same instant. Postgres'
    // 23505 (unique_violation) on idx_pairings_unique_active_pair
    // (migration 011) is the real guarantee; surface it as the same
    // friendly error rather than a bare 500.
    if (err.code === '23505' && err.constraint === 'idx_pairings_unique_active_pair') {
      return {
        status: 409,
        body: { error: 'you and this account are already paired up' },
      };
    }
    throw err;
  } finally {
    client.release();
  }
}

// Redeem someone else's invite code directly (they sent it to you).
invitesRouter.post('/invites/:code/redeem', requireAuth, requireEmailVerified, redeemPerAccount, async (req, res) => {
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
//
// Pairs are meant to trade like-for-like: someone who needs 5GB redeeming
// a 20TB listing isn't a match, it's a 20TB obligation (pledged_bytes is
// shared by both sides of the resulting pairing — see redeemInvite) that
// nobody asked for. So this always filters to one exact size rather than
// returning every listing — ?pledged_bytes=<bytes> picks which size tier
// to show, same as the "Storage to pledge" dropdown's values. It's
// required, not optional, because an unfiltered "browse everything" view
// is exactly the mismatch this is meant to prevent; the dashboard always
// sends it.
//
// Paginated (?offset=, ?limit=) rather than returned all at once — a
// single size tier could in principle grow into hundreds of listings, and
// find-a-buddy.html pages through them with a "Load more" button instead
// of ever asking for all of them in one response. Also includes, per
// listing, the inviter's self-reported country/speed_tier/always_on/
// relay_only, plus two things derived from the inviter's own account
// history rather than self-reported: how long they've had an account, and
// how many pairings (of any outcome — active or revoked) they've actually
// completed before. Not a trust/rating system, just a lightweight signal
// a browser can use the same way they'd eyeball an account's age on any
// other marketplace.
//
// Optional ?country=<exact string> narrows to that one self-reported
// country (an exact match against COUNTRY_OPTIONS in pledge-options.js,
// same list the invite-creation form writes from — not a partial/ILIKE
// match, since the field is a closed dropdown, not free text). This is
// the first of the four self-reported fields (country/speed_tier/
// always_on/relay_only) to become a real filter rather than display-only
// — see docs/TESTING-CHECKLIST.md's "Marketplace listing filters" entry;
// speed/always_on/relay_only stay display-only for now.
invitesRouter.get('/invites/public', requireAuth, async (req, res) => {
  const pledgedBytes = Number(req.query.pledged_bytes);
  if (!Number.isInteger(pledgedBytes) || pledgedBytes <= 0) {
    return res.status(400).json({ error: 'pledged_bytes query parameter is required' });
  }
  const limit = req.query.limit !== undefined ? Number(req.query.limit) : MARKETPLACE_PAGE_SIZE;
  const offset = req.query.offset !== undefined ? Number(req.query.offset) : 0;
  if (!Number.isInteger(limit) || limit <= 0 || limit > MARKETPLACE_MAX_PAGE_SIZE) {
    return res.status(400).json({ error: `limit must be between 1 and ${MARKETPLACE_MAX_PAGE_SIZE}` });
  }
  if (!Number.isInteger(offset) || offset < 0) {
    return res.status(400).json({ error: 'offset must be 0 or greater' });
  }
  // Empty string (an unselected "Any country" filter) is the same as not
  // filtering at all, not a search for invites with an empty country.
  const country = typeof req.query.country === 'string' && req.query.country.length > 0
    ? req.query.country
    : null;

  const result = await pool.query(
    `SELECT invites.id, invites.pledged_bytes, invites.note, invites.created_at,
            invites.country, invites.speed_tier, invites.always_on, invites.relay_only,
            COALESCE(accounts.nickname, 'Buddy #' || substring(accounts.id::text, 1, 8)) AS inviter_label,
            accounts.created_at AS inviter_member_since,
            (SELECT count(*) FROM buddy_pairings
              WHERE (account_a_id = accounts.id OR account_b_id = accounts.id)
                AND status IN ('active', 'revoked')) AS inviter_completed_pairings,
            count(*) OVER () AS total_count
       FROM invites
       JOIN accounts ON accounts.id = invites.inviter_account_id
      WHERE invites.is_public = true
        AND invites.status = 'pending'
        AND invites.inviter_account_id <> $1
        AND invites.pledged_bytes = $2
        AND ($5::text IS NULL OR invites.country = $5)
      ORDER BY invites.created_at DESC
      LIMIT $3 OFFSET $4`,
    [req.account.id, pledgedBytes, limit, offset, country],
  );
  const totalCount = result.rows[0] ? Number(result.rows[0].total_count) : 0;
  const listings = result.rows.map(({ total_count, ...listing }) => listing);

  res.json({
    listings,
    pledged_bytes: pledgedBytes,
    total_count: totalCount,
    offset,
    limit,
    has_more: offset + listings.length < totalCount,
  });
});

// Request a pairing from a public listing — same effect as redeeming a
// code, just looked up by listing id instead of needing the code texted
// to you directly. pledged_bytes in the body is what the redeemer saw and
// clicked on in "Find a buddy"; redeemInvite re-checks it against the
// invite's actual size before pairing (see the comment there).
invitesRouter.post('/invites/public/:id/redeem', requireAuth, requireEmailVerified, redeemPerAccount, async (req, res) => {
  const expectedPledgedBytes = Number(req.body?.pledged_bytes);
  if (!Number.isInteger(expectedPledgedBytes) || expectedPledgedBytes <= 0) {
    return res.status(400).json({ error: 'pledged_bytes is required in the request body' });
  }

  const result = await redeemInvite({
    lockQuery: `SELECT * FROM invites WHERE id = $1 AND is_public = true FOR UPDATE`,
    lockParam: req.params.id,
    redeemerAccountId: req.account.id,
    expectedPledgedBytes,
  });
  res.status(result.status).json(result.body);
});
