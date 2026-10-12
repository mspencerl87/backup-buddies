import { Router } from 'express';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';
import { stripe } from '../lib/stripe.js';
import { RELAY_USD_PER_GB, FREE_RELAY_GB_PER_MONTH, relayBytesThisMonth } from '../lib/relay-billing.js';

export const billingRouter = Router();

// Also enforced (not just reported) in invites.js — the first 50GB an
// account has pledged, across all its active pairings, costs nothing;
// pledging past that requires an active Stripe subscription.
export const FREE_GB = 50;
const BYTES_PER_GB = 1024 ** 3;

// The paid plan is a flat $3/month for any pledge size (STRIPE_STORAGE_PRICE_ID,
// see scripts/create-stripe-storage-price.mjs). It used to be $3/TB/month on
// what you'd pledged, with the subscription quantity tracking pledged GB, but
// a pairing is a symmetric swap of disks people supply themselves, and paying
// per TB for it read as renting your own disk (feedback on the launch post).
// Direct transfers cost us nothing at any size; the per-account service and
// the relay are what cost money, so those are what get charged for.
export const PLAN_USD_PER_MONTH = 3;

// How much storage this account is actually committed to right now, across
// every active buddy pairing it's part of (on either side — pledging and
// being pledged to are symmetric). No longer what's billed, but still what
// decides whether an account fits in the free tier.
//
// Exported so invites.js can enforce the free allowance at the point a
// pledge is actually made (invite creation and redemption), not just
// report it after the fact here.
export async function pledgedGBForAccount(accountId) {
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

billingRouter.get('/billing/summary', requireAuth, async (req, res) => {
  const pledgedGB = await pledgedGBForAccount(req.account.id);

  res.json({
    pledged_gb: pledgedGB,
    free_allowance_gb: FREE_GB,
    plan_usd_per_month: PLAN_USD_PER_MONTH,
    has_subscription: Boolean(req.account.stripe_subscription_id),
    // Complimentary account (set by an admin, see routes/admin.js): no
    // card needed, so the dashboard shows that instead of Subscribe.
    billing_exempt: Boolean(req.account.billing_exempt),
    relay_status: req.account.relay_status,
    // Relay-bandwidth billing is paid-tier only (bundled into the same
    // checkout/subscription as storage below) — these let the dashboard
    // show the rate and whether it's actually live on this deployment.
    relay_usd_per_gb: RELAY_USD_PER_GB,
    relay_billing_enabled: Boolean(process.env.STRIPE_RELAY_PRICE_ID),
    // The monthly relay allowance (lib/relay-billing.js), included on both
    // plans, and how much of it is used so far this month. Past it, free
    // accounts pause and paid ones pay relay_usd_per_gb.
    free_relay_gb_per_month: FREE_RELAY_GB_PER_MONTH,
    relay_bytes_this_month: await relayBytesThisMonth(req.account.id),
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
  // The flat plan: quantity is always 1, however much is pledged.
  const lineItems = [{ price: process.env.STRIPE_STORAGE_PRICE_ID, quantity: 1 }];
  // Bundled into the same subscription as storage, as a second line item —
  // one invoice, one subscription to manage. Only added when this
  // deployment has actually created the relay Price (see
  // scripts/create-stripe-relay-meter-and-price.mjs); no quantity here
  // since it's a metered price billed from reported usage (see
  // relay-billing.js / jobs/sync-relay-usage.js), not a line-item quantity.
  if (process.env.STRIPE_RELAY_PRICE_ID) {
    lineItems.push({ price: process.env.STRIPE_RELAY_PRICE_ID });
  }

  const session = await stripe.checkout.sessions.create({
    mode: 'subscription',
    customer: customerId,
    client_reference_id: req.account.id,
    line_items: lineItems,
    // Stripe's "Managed Payments" (enabled by default on newer accounts)
    // requires every product to carry a tax code before Checkout will let
    // it through. Not something this project needs to deal with yet —
    // disable it explicitly rather than assigning a tax code we'd have to
    // get right for a storage-trading subscription product.
    managed_payments: { enabled: false },
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
