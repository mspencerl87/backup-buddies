-- Free accounts now get a monthly relay allowance (lib/relay-billing.js):
-- every relay usage report from a free account sums that account's
-- relay_usage_events for the current month, and so do /billing/summary and
-- the admin account view. The existing idx_relay_usage_unsynced only
-- covers unsynced rows, and free accounts' rows are written as synced
-- (they're never billed), so without this those sums scan the table.
--
-- Additive and safe to apply any time, before or after the code ships —
-- the code is correct without it, just slower as the table grows.
CREATE INDEX IF NOT EXISTS idx_relay_usage_account_time
  ON relay_usage_events (account_id, occurred_at);
