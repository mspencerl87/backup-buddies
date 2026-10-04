-- Password reset links, sent from the admin account-detail view
-- (POST /admin/accounts/:id/password-reset) or self-service from
-- forgot-password.html (POST /auth/forgot-password), and consumed by
-- apps/web/reset-password.html (POST /auth/reset-password).
--
-- Same pattern as email verification (migration 007): only the SHA-256 of
-- the token is stored, never the raw token, and it's cleared on use so the
-- emailed link can't be replayed. password_reset_sent_at doubles as the
-- expiry anchor (see lib/password-reset.js's TOKEN_TTL_HOURS).
--
-- Additive and nullable — safe to apply before or after the code that
-- reads it ships, but the admin "Send password reset" button and the reset
-- page will 500 until it's applied.
ALTER TABLE accounts
  ADD COLUMN password_reset_token_hash TEXT,
  ADD COLUMN password_reset_sent_at TIMESTAMPTZ;

CREATE INDEX idx_accounts_password_reset_token_hash
  ON accounts (password_reset_token_hash)
  WHERE password_reset_token_hash IS NOT NULL;
