import { Router } from 'express';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';
import { stripe } from '../lib/stripe.js';

export const billingRouter = Router();

const FREE_GB = 50;
const BYTES_PER_GB = 1024 ** 3;
const USD_PER_TB_MONTH = 3;

// How much storage this account is actually committed to right now, across
// every active buddy pairing it's part of (on either side — pledging and
// being pledged to are symmetric). This is what gets billed, per the
// "billed on pledged storage" model: a subscription quantity that only
// moves when a pairing is created or ends, not a live usage meter.
async function pledgedGBForAccount(accountId) {
  const result = await pool.query(
    `SELECT COALESCE(SUM(pledged_bytes), 0) AS total_bytes
       FROM buddy_pairings
      WHERE (account_a_id = $1 OR account_b_id = $1)
        AND status = 'active'`,
    [accountId],
  );
  return Math.ceil(Number(result.rows[0].total_bytes) / BYTES_PER_GB);
}

async function getOrCreateStripeCustomer(account) {
  if (account.stripe_customer_id) return account.stripe_customer_id;

  const customer = await stripe.customers.create({
    email: account.email,
    metadata: { account_id: account.id },
  });
  await pool.query('UPDATE accounts SET stripe_customer_id = $1 WHERE id = $2', [
    customer.id,
    account.id,
  ]);
  return customer.id;
}

// Called after a pairing is created or ends: brings the account's live
// Stripe subscription quantity (if it has one — this is a no-op for an
// account that hasn't subscribed yet) up to date with its current total
// pledge. Never throws — a Stripe hiccup here shouldn't break the caller
// (e.g. a successful invite redemption).
export async function syncSubscriptionQuantity(accountId) {
  const result = await pool.query(
    'SELECT stripe_subscription_item_id FROM accounts WHERE id = $1',
    [accountId],
  );
  const itemId = result.rows[0]?.stripe_subscription_item_id;
  if (!itemId) return;

  try {
    const quantity = Math.max(1, await pledgedGBForAccount(accountId));
    await stripe.subscriptionItems.update(itemId, {
      quantity,
      proration_behavior: 'create_prorations',
    });
  } catch (err) {
    console.error(`Failed to sync Stripe subscription quantity for account ${accountId}:`, err);
  }
}

billingRouter.get('/billing/summary', requireAuth, async (req, res) => {
  const pledgedGB = await pledgedGBForAccount(req.account.id);
  const billableGB = Math.max(0, pledgedGB - FREE_GB);

  res.json({
    pledged_gb: pledgedGB,
    free_allowance_gb: FREE_GB,
    billable_gb: billableGB,
    // Informational estimate only — the Stripe subscription (once created)
    // is the actual source of truth for what gets charged.
    estimated_monthly_usd: Number(((billableGB * USD_PER_TB_MONTH) / 1024).toFixed(2)),
    has_subscription: Boolean(req.account.stripe_subscription_id),
    relay_status: req.account.relay_status,
  });
});

billingRouter.post('/billing/checkout', requireAuth, async (req, res) => {
  if (!process.env.STRIPE_STORAGE_PRICE_ID || !process.env.APP_URL) {
    return res.status(503).json({ error: 'billing is not configured yet' });
  }
  if (req.account.stripe_subscription_id) {
    return res
      .status(409)
      .json({ error: 'you already have a subscription — use Manage billing instead' });
  }

  const customerId = await getOrCreateStripeCustomer(req.account);
  // Quantity starts at the account's current pledge (minimum 1 — Stripe
  // line items can't be zero). If that's under the 50GB free allowance the
  // tiered price charges $0, which is fine: this just gets billing set up
  // and ready to scale automatically as pledges grow.
  const quantity = Math.max(1, await pledgedGBForAccount(req.account.id));

  const session = await stripe.checkout.sessions.create({
    mode: 'subscription',
    customer: customerId,
    client_reference_id: req.account.id,
    line_items: [{ price: process.env.STRIPE_STORAGE_PRICE_ID, quantity }],
    success_url: `${process.env.APP_URL}/dashboard.html?checkout=success`,
    cancel_url: `${process.env.APP_URL}/dashboard.html?checkout=cancelled`,
  });

  res.json({ url: session.url });
});

billingRouter.post('/billing/portal', requireAuth, async (req, res) => {
  if (!req.account.stripe_customer_id || !process.env.APP_URL) {
    return res.status(400).json({ error: 'no billing account yet' });
  }

  const session = await stripe.billingPortal.sessions.create({
    customer: req.account.stripe_customer_id,
    return_url: `${process.env.APP_URL}/dashboard.html`,
  });

  res.json({ url: session.url });
});
