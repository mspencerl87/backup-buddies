-- invites.redeemed_by_account_id referenced accounts(id) with no ON DELETE
-- action (the only FK in the schema missing one), so deleting an account
-- that had ever redeemed an invite -- even one whose buddy_pairings row
-- was since removed, leaving just the invite behind -- failed with a
-- foreign key violation (23503) reported to the admin UI as a bare
-- "internal server error". The invite record belongs to the inviter, not
-- the redeemer, so it shouldn't vanish just because the redeemer's account
-- does -- SET NULL keeps the invite (and its inviter, pledge, and
-- redeemed_at history) while clearing the now-gone redeemer reference,
-- rather than CASCADE deleting someone else's invite.

ALTER TABLE invites DROP CONSTRAINT invites_redeemed_by_account_id_fkey;
ALTER TABLE invites
  ADD CONSTRAINT invites_redeemed_by_account_id_fkey
  FOREIGN KEY (redeemed_by_account_id) REFERENCES accounts(id) ON DELETE SET NULL;
