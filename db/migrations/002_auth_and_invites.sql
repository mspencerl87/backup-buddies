-- Adds password auth, sessions, and invite-code buddy pairing.
--
-- init.sql only runs automatically on a brand-new Postgres data volume, so
-- this file needs to be applied by hand against the already-running
-- database — see docs/DEPLOY.md for the exact command. It's written to be
-- safe to run once against the schema created by db/init.sql.

ALTER TABLE accounts ADD COLUMN password_hash TEXT NOT NULL DEFAULT '';

CREATE TABLE sessions (
  id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  account_id  UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at  TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_sessions_account ON sessions (account_id);

-- One-time codes an account generates to invite a specific person to
-- become their buddy. Redeeming a code creates the actual buddy_pairings
-- row (already defined in init.sql).
CREATE TABLE invites (
  id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  code                    TEXT NOT NULL UNIQUE,
  inviter_account_id      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  pledged_bytes           BIGINT NOT NULL,
  status                  TEXT NOT NULL DEFAULT 'pending',
    -- 'pending' | 'redeemed' | 'revoked'
  created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
  redeemed_by_account_id  UUID REFERENCES accounts(id),
  redeemed_at             TIMESTAMPTZ
);

CREATE INDEX idx_invites_code ON invites (code);
CREATE INDEX idx_invites_inviter ON invites (inviter_account_id);
