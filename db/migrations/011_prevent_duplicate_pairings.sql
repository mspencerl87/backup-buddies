-- Nothing stopped two accounts from ending up with two (or more) active
-- buddy_pairings rows between them -- e.g. A invites B and separately B
-- invites A, and both get redeemed. Each row is independently "symmetric"
-- (same pledge either way, per the comment on the table) but a second row
-- between the same pair isn't a second relationship, it's the same
-- relationship pledged twice -- it double-counts toward both accounts'
-- 50GB free-allowance checks and billing quantity, shows as a confusing
-- duplicate row on the dashboard, and gives the client two buddy
-- connections to reconcile against instead of one.
--
-- This is enforced at the API layer too (see redeemInvite in
-- invites.js), but that check-then-insert isn't safe against two
-- concurrent redemptions racing each other, so the real guarantee needs
-- to live here. A plain UNIQUE(account_a_id, account_b_id) wouldn't catch
-- it either, since a second invite redeemed in the other direction
-- inserts (B, A), not (A, B) -- LEAST/GREATEST normalizes the pair so
-- both orderings collide on the same index entry.
--
-- Scoped to status = 'active': a 'revoked' pairing shouldn't block the
-- same two people from pairing again later, and 'pending' isn't a real
-- status buddy_pairings rows are ever inserted with (see invites.js --
-- redemption always inserts 'active' directly), so excluding it here
-- just documents that rather than changing behavior.
--
-- Existing double-pairings (this is live data, not a fresh install --
-- testing already produced at least one pair with two active rows)
-- would make the CREATE UNIQUE INDEX below fail outright, so clean those
-- up first: keep each pair's oldest active row and revoke the rest,
-- same as if one side had manually ended the extra pairing. Revoke
-- rather than delete so the history (and whatever the client already
-- reconciled against it) isn't silently erased.
WITH ranked AS (
  SELECT id,
         row_number() OVER (
           PARTITION BY LEAST(account_a_id, account_b_id), GREATEST(account_a_id, account_b_id)
           ORDER BY created_at, id
         ) AS rn
    FROM buddy_pairings
   WHERE status = 'active'
)
UPDATE buddy_pairings
   SET status = 'revoked'
  FROM ranked
 WHERE buddy_pairings.id = ranked.id
   AND ranked.rn > 1;

CREATE UNIQUE INDEX idx_pairings_unique_active_pair
  ON buddy_pairings (LEAST(account_a_id, account_b_id), GREATEST(account_a_id, account_b_id))
  WHERE status = 'active';
