-- Supports billing by pledged storage (option (a): a subscription quantity
-- tied to buddy_pairings.pledged_bytes, not live usage metering).
-- stripe_customer_id and stripe_subscription_id already exist from
-- db/init.sql; this adds the one thing missing — the specific subscription
-- item whose quantity gets updated whenever a pairing changes.

ALTER TABLE accounts ADD COLUMN stripe_subscription_item_id TEXT;
