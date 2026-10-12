#!/usr/bin/env node
// One-shot setup script: creates the Stripe Product + flat Price for the
// paid plan ($3/month, any pledge size; the 50GB free tier needs no Stripe
// subscription at all), and prints the resulting Price ID to put into .env
// as STRIPE_STORAGE_PRICE_ID. Before the flat plan this created a tiered
// per-GB price (50GB free, then $3/TB/month); subscriptions on that one are
// moved over by scripts/move-subscriptions-to-flat-plan.mjs.
//
// Run with STRIPE_SECRET_KEY set in your shell — no npm install needed,
// this talks to Stripe's REST API directly over fetch (built into Node 20+):
//
//   STRIPE_SECRET_KEY=sk_test_xxx node scripts/create-stripe-storage-price.mjs
//
// Safe to run more than once — each run creates a NEW Product/Price (Stripe
// Prices are immutable once created), so if you re-run this to fix a typo,
// update STRIPE_STORAGE_PRICE_ID to the newest one it prints.

const secretKey = process.env.STRIPE_SECRET_KEY;
if (!secretKey) {
  console.error('Set STRIPE_SECRET_KEY in your shell before running this script.');
  process.exit(1);
}

// Must match PLAN_USD_PER_MONTH in apps/api/src/routes/billing.js.
const USD_PER_MONTH = 3;

async function stripeRequest(path, params) {
  const body = new URLSearchParams();
  for (const [key, value] of Object.entries(flatten(params))) {
    body.append(key, value);
  }

  const res = await fetch(`https://api.stripe.com/v1/${path}`, {
    method: 'POST',
    headers: {
      Authorization: `Basic ${Buffer.from(`${secretKey}:`).toString('base64')}`,
      'Content-Type': 'application/x-www-form-urlencoded',
    },
    body,
  });

  const json = await res.json();
  if (!res.ok) {
    throw new Error(`Stripe API error (${path}): ${json.error?.message ?? JSON.stringify(json)}`);
  }
  return json;
}

// Turns { recurring: { interval: 'month' }, tiers: [{ up_to: 50 }] } into
// Stripe's bracket-notation form fields: "recurring[interval]" -> "month",
// "tiers[0][up_to]" -> "50".
function flatten(obj, prefix = '') {
  const out = {};
  for (const [key, value] of Object.entries(obj)) {
    const field = prefix ? `${prefix}[${key}]` : key;
    if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
      Object.assign(out, flatten(value, field));
    } else if (Array.isArray(value)) {
      value.forEach((item, i) => Object.assign(out, flatten(item, `${field}[${i}]`)));
    } else {
      out[field] = String(value);
    }
  }
  return out;
}

console.log(`Creating a flat Price: $${USD_PER_MONTH}/month...`);

// A new Product rather than the old "Backup Buddies Storage" one: this is
// the name people see on their invoices, and they aren't paying for storage.
const price = await stripeRequest('prices', {
  currency: 'usd',
  unit_amount: USD_PER_MONTH * 100,
  recurring: { interval: 'month', usage_type: 'licensed' },
  product_data: { name: 'Backup Buddies plan' },
});

console.log('\nDone. Add this to .env on app-1:\n');
console.log(`STRIPE_STORAGE_PRICE_ID=${price.id}`);
console.log(`\n(Product: ${price.product}, Price: ${price.id})`);
