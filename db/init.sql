-- Backup Buddies — control-plane schema
-- This database holds accounts, pairings, quotas, and billing refs only.
-- No user file data or file metadata is ever stored here.

CREATE EXTENSION IF NOT EXISTS pgcrypto; -- for gen_random_uuid()

CREATE TABLE accounts (
  id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  email                   TEXT NOT NULL UNIQUE,
  created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),

  tier                    TEXT NOT NULL DEFAULT 'free',
    -- 'free' | 'starter' | 'standard' | 'plus' | 'pro'

  stripe_customer_id      TEXT UNIQUE,
  stripe_subscription_id  TEXT UNIQUE,

  relay_status            TEXT NOT NULL DEFAULT 'active'
    -- 'active' | 'paused'  (paused = free-tier relay allowance exhausted,
    -- or a paid account with a failed payment)
);

-- One row per physical device/client install. A single account can run the
-- agent on more than one machine.
CREATE TABLE devices (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  account_id      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  iroh_node_id    TEXT NOT NULL UNIQUE,
  label           TEXT, -- e.g. "Spencer's desktop"
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_seen_at    TIMESTAMPTZ
);

-- A matched buddy pair. pledged_bytes is symmetric by design (A pledges to B
-- what B pledges to A) but stored per-direction in case that ever changes.
CREATE TABLE buddy_pairings (
  id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  account_a_id      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  account_b_id      UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  pledged_bytes     BIGINT NOT NULL,
  status            TEXT NOT NULL DEFAULT 'pending',
    -- 'pending' | 'active' | 'revoked'
  created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),

  CONSTRAINT different_accounts CHECK (account_a_id <> account_b_id)
);

-- Raw relay bandwidth events. Written by the API (POST /devices/me/buddies,
-- see routes/devices.js / lib/relay-billing.js) as each client reports the
-- relay megabytes it's moved since its last report — the relay server
-- itself is the stock iroh relay binary and has no way to emit these.
-- Aggregated and reported to Stripe Billing Meters on a schedule
-- (jobs/sync-relay-usage.js), then marked synced.
CREATE TABLE relay_usage_events (
  id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  account_id        UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  bytes_relayed     BIGINT NOT NULL,
  occurred_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  synced_to_stripe  BOOLEAN NOT NULL DEFAULT false
);

CREATE INDEX idx_relay_usage_unsynced
  ON relay_usage_events (account_id)
  WHERE synced_to_stripe = false;

CREATE INDEX idx_devices_account ON devices (account_id);
CREATE INDEX idx_pairings_account_a ON buddy_pairings (account_a_id);
CREATE INDEX idx_pairings_account_b ON buddy_pairings (account_b_id);
