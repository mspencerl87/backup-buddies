#!/usr/bin/env node
// One-shot migration script: moves every live subscription still on the old
// tiered storage price ($3/TB/month past 50GB, quantity = pledged GB) onto
// the flat $3/month plan price. Same subscription item, swapped to the new
// price with quantity 1, so accounts.stripe_subscription_item_id stays valid
// and nothing in the database needs touching.
//
// No proration: the tiered price was billed in advance, so the period each
// person has already paid for stands, and the flat price starts at their
// next renewal. (The admin page's "Move to flat plan" button does the same
// for one account.)
//
// Dry run by default: lists each subscription with what it pays now and
// what it'll pay after, so you can see who goes up (anyone pledging under
// about 1.07TB, who paid less than $3) before telling them. Add --apply to
// actually change anything. Run after scripts/create-stripe-storage-price.mjs,
// with the OLD tiered price id and the NEW flat one:
//
//   STRIPE_SECRET_KEY=sk_live_xxx \
//   OLD_STORAGE_PRICE_ID=price_old \
//   STRIPE_STORAGE_PRICE_ID=price_new \
//   node scripts/move-subscriptions-to-flat-plan.mjs [--apply]
//
// Talks to Stripe only (no database access needed). Safe to run more than
// once: subscriptions already on the new price aren't listed by the old one.

const secretKey = process.env.STRIPE_SECRET_KEY;
const oldPriceId = process.env.OLD_STORAGE_PRICE_ID;
const newPriceId = process.env.STRIPE_STORAGE_PRICE_ID;
const apply = process.argv.includes('--apply');
if (!secretKey || !oldPriceId || !newPriceId) {
  console.error(
    'Set STRIPE_SECRET_KEY, OLD_STORAGE_PRICE_ID and STRIPE_STORAGE_PRICE_ID in your shell before running this script.',
  );
  process.exit(1);
}
if (oldPriceId === newPriceId) {
  console.error('OLD_STORAGE_PRICE_ID and STRIPE_STORAGE_PRICE_ID are the same price.');
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

const newPrice = await stripeGet(`prices/${newPriceId}`);
const newMonthlyUsd = newPrice.unit_amount / 100;

// Same as the old price's tiers: first 50GB free, then $3/TB.
function oldMonthlyUsd(quantityGb) {
  return (Math.max(0, quantityGb - 50) * 3) / 1024;
}

// Everything Stripe still bills (or is retrying). 'all' rather than
// 'active' so past_due and unpaid ones get moved too.
const LIVE = new Set(['active', 'trialing', 'past_due', 'unpaid']);

let startingAfter;
let found = 0;
let moved = 0;
let goingUp = 0;

for (;;) {
  const qs = new URLSearchParams({ price: oldPriceId, status: 'all', limit: '100' });
  qs.append('expand[]', 'data.customer');
  if (startingAfter) qs.set('starting_after', startingAfter);
  const page = await stripeGet(`subscriptions?${qs}`);

  for (const subscription of page.data) {
    if (!LIVE.has(subscription.status)) continue;
    const item = subscription.items.data.find((i) => i.price?.id === oldPriceId);
    if (!item) continue;
    found += 1;

    const before = oldMonthlyUsd(item.quantity ?? 0);
    if (before < newMonthlyUsd) goingUp += 1;
    const email = subscription.customer?.email ?? subscription.customer?.id ?? subscription.customer;
    console.log(
      `${subscription.id} (${email}, ${subscription.status}): ${item.quantity} GB pledged, ` +
        `$${before.toFixed(2)}/month now -> $${newMonthlyUsd.toFixed(2)}/month` +
        (before < newMonthlyUsd ? '  [goes up]' : ''),
    );

    if (apply) {
      await stripePost(`subscription_items/${item.id}`, {
        price: newPriceId,
        quantity: 1,
        proration_behavior: 'none',
      });
      moved += 1;
    }
  }

  if (!page.has_more || page.data.length === 0) break;
  startingAfter = page.data[page.data.length - 1].id;
}

console.log(
  `\n${found} live subscription(s) on the old price, ${goingUp} of them paying less than ` +
    `$${newMonthlyUsd.toFixed(2)} now.`,
);
console.log(apply ? `Moved ${moved} to the flat plan from their next renewal.` : 'Dry run: nothing changed. Add --apply to move them.');
