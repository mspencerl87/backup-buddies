-- Device API tokens + public marketplace listings.
--
-- Apply by hand against the running database (init.sql only runs on a
-- fresh volume) — see docs/DEPLOY.md.

-- The headless backup-client agent authenticates with a bearer token, not
-- a browser session cookie. A device row is now created from the dashboard
-- (session-authed) *before* the client has run even once — its Iroh node
-- id isn't known until the client's first check-in generates its keypair,
-- so iroh_node_id has to become optional at creation time.
ALTER TABLE devices ALTER COLUMN iroh_node_id DROP NOT NULL;
ALTER TABLE devices ADD COLUMN token_hash TEXT UNIQUE;
ALTER TABLE devices ADD COLUMN token_last_four TEXT;

CREATE INDEX idx_devices_token_hash ON devices (token_hash) WHERE token_hash IS NOT NULL;

-- Public marketplace: same underlying mechanism as a private invite code,
-- just browsable by any other account instead of requiring the code to be
-- sent directly. Matching still produces a normal symmetric buddy_pairings
-- row — the marketplace is a different way to find who to pair with, not
-- a different trading model.
ALTER TABLE invites ADD COLUMN is_public BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE invites ADD COLUMN note TEXT;

CREATE INDEX idx_invites_public ON invites (is_public)
  WHERE is_public = true AND status = 'pending';

-- Optional public display name so marketplace listings don't show other
-- accounts' raw email addresses. Falls back to a short account-id-based
-- label when unset (handled in the API, not here).
ALTER TABLE accounts ADD COLUMN nickname TEXT;
