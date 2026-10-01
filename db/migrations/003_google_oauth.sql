-- Adds Google sign-in alongside the existing email/password accounts.
-- password_hash becomes optional: an account created via Google has no
-- password at all until (if ever) they set one.

ALTER TABLE accounts ALTER COLUMN password_hash DROP NOT NULL;
ALTER TABLE accounts ALTER COLUMN password_hash DROP DEFAULT;

-- Backfill: turn the '' default used by existing rows into a real NULL so
-- "has a password" can be checked with a plain IS NOT NULL everywhere.
UPDATE accounts SET password_hash = NULL WHERE password_hash = '';

ALTER TABLE accounts ADD COLUMN google_sub TEXT UNIQUE;

CREATE INDEX idx_accounts_google_sub ON accounts (google_sub) WHERE google_sub IS NOT NULL;
