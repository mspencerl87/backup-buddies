-- Email verification, to make bot/throwaway signups more friction than
-- they're worth. New password-signup accounts start unverified and can't
-- create or redeem invites (pairing) until they click the link sent to
-- their address — see middleware/require-email-verified.js and
-- lib/email-verification.js. They can still log in and look around; this
-- only gates pairing, nothing else.
--
-- Google accounts never go through this at all: routes/auth.js already
-- requires payload.email_verified from Google before creating or linking
-- an account, which is at least as strong a guarantee as our own token
-- flow, so those accounts are marked verified immediately at signup/link
-- time.
ALTER TABLE accounts
  ADD COLUMN email_verified BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN email_verification_token_hash TEXT,
  ADD COLUMN email_verification_sent_at TIMESTAMPTZ;

-- Backfill: every account that already exists has a track record of
-- actually using the service — this is about raising the bar for *new*
-- signups going forward, not retroactively locking existing users out of
-- pairing.
UPDATE accounts SET email_verified = true;

-- Only the hash is ever stored (same pattern as devices.token_hash) — the
-- raw token lives only in the emailed link. Partial index since most rows
-- will have no pending token at all (already verified, or never requested
-- a fresh link).
CREATE INDEX idx_accounts_email_verification_token_hash
  ON accounts (email_verification_token_hash)
  WHERE email_verification_token_hash IS NOT NULL;
