#!/usr/bin/env node
// One-shot migration script: adds the relay-bandwidth metered Price to
// every active Stripe subscription that doesn't already have it — for
// accounts that subscribed to storage billing before relay billing
// existed. New subscriptions get both items from the start (see
// apps/api/src/routes/billing.js); this just backfills ones created
// earlier so relay usage actually gets billed for them too.
//
// Run after scripts/create-stripe-relay-meter-and-price.mjs, with
// STRIPE_SECRET_KEY and both price ids set:
//
//   STRIPE_SECRET_KEY=sk_live_xxx \
//   STRIPE_STORAGE_PRICE_ID=price_xxx \
//   STRIPE_RELAY_PRICE_ID=price_yyy \
//   node scripts/add-relay-price-to-existing-subscriptions.mjs
//
// Talks to Stripe only (no database access needed) — it lists every active
// subscription that carries the storage price, and for each one not
// already carrying the relay price, adds it as a new subscription item.
// Safe to run more than once: already-updated subscriptions are skipped.

const secretKey = process.env.STRIPE_SECRET_KEY;
const storagePriceId = process.env.STRIPE_STORAGE_PRICE_ID;
const relayPriceId = process.env.STRIPE_RELAY_PRICE_ID;
if (!secretKey || !storagePriceId || !relayPriceId) {
  console.error(
    'Set STRIPE_SECRET_KEY, STRIPE_STORAGE_PRICE_ID, and STRIPE_RELAY_PRICE_ID in your shell before running this script.',
  );
  process.exit(1);
}

const authHeader = { Authorization: `Basic ${Buffer.from(`${secretKey}:`).toString('base64')}` };

async function stripeGet(path) {
  const res = await fetch(`https://api.stripe.com/v1/${path}`, { headers: authHeader });
  const json = await res.json();
  if (!res.ok) {
    throw new Error(`Stripe API error (${path}): ${json.error?.message ?? JSON.stringify(json)}`);
  }
  return json;
}

async function stripePost(path, params) {
  const body = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) body.append(key, String(value));

  const res = await fetch(`https://api.stripe.com/v1/${path}`, {
    method: 'POST',
    headers: { ...authHeader, 'Content-Type': 'application/x-www-form-urlencoded' },
    body,
  });
  const json = await res.json();
  if (!res.ok) {
    throw new Error(`Stripe API error (${path}): ${json.error?.message ?? JSON.stringify(json)}`);
  }
  return json;
}

let startingAfter;
let checked = 0;
let added = 0;

for (;;) {
  const qs = new URLSearchParams({ price: storagePriceId, status: 'active', limit: '100' });
  if (startingAfter) qs.set('starting_after', startingAfter);
  const page = await stripeGet(`subscriptions?${qs}`);

  for (const subscription of page.data) {
    checked += 1;
    const alreadyHasRelay = subscription.items.data.some(
      (item) => item.price?.id === relayPriceId,
    );
    if (alreadyHasRelay) continue;

    await stripePost('subscription_items', {
      subscription: subscription.id,
      price: relayPriceId,
    });
    added += 1;
    console.log(`Added relay price to subscription ${subscription.id}`);
  }

  if (!page.has_more || page.data.length === 0) break;
  startingAfter = page.data[page.data.length - 1].id;
}

console.log(`\nDone. Checked ${checked} active storage subscription(s), added the relay price to ${added}.`);
