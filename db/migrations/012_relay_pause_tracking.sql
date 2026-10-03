-- relay_status already exists on accounts ('active' | 'paused'), but until
-- now nothing recorded *when* an account was paused or *why* -- it was set
-- by the invoice.payment_failed webhook handler and never read anywhere
-- else in the codebase (relay_status was purely cosmetic). Enforcing it
-- (stripe-webhook.js, relay-billing.js, devices.js, the Rust client) means
-- an admin now needs to be able to see which accounts are paused and for
-- how long, to decide whether to reach out, wait, or clean up -- hence
-- these two columns.
--
-- relay_paused_reason is free text rather than an enum: today it's always
-- 'payment_failed', but relay_status's own comment already anticipates a
-- second reason (free-tier allowance exhausted) that isn't implemented
-- yet, and a text column avoids a second migration just to add a value.
ALTER TABLE accounts
  ADD COLUMN relay_paused_at TIMESTAMPTZ,
  ADD COLUMN relay_paused_reason TEXT;

-- Lets the admin accounts list (sorted by "paused longest") and any future
-- "accounts paused > N days" query run without a sequential scan.
CREATE INDEX idx_accounts_relay_paused_at
  ON accounts (relay_paused_at)
  WHERE relay_paused_at IS NOT NULL;
