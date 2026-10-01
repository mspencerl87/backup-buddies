#!/usr/bin/env node
// One-shot setup script: creates the Stripe Product + tiered Price for
// storage billing (50GB free, then $3/TB/month), and prints the resulting
// Price ID to put into .env as STRIPE_STORAGE_PRICE_ID.
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

// $3 / 1024 GB per TB = exact price per GB, expressed in fractional cents
// so there's no rounding error accumulated across quantities.
const USD_PER_TB = 3;
const CENTS_PER_GB = ((USD_PER_TB * 100) / 1024).toFixed(8); // "0.29296875"
const FREE_GB_TIER = 50;

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

console.log(`Creating a tiered Price: first ${FREE_GB_TIER}GB free, then $${USD_PER_TB}/TB/month (${CENTS_PER_GB}¢/GB)...`);

const price = await stripeRequest('prices', {
  currency: 'usd',
  billing_scheme: 'tiered',
  tiers_mode: 'graduated',
  tiers: [
    { up_to: FREE_GB_TIER, unit_amount_decimal: '0' },
    { up_to: 'inf', unit_amount_decimal: CENTS_PER_GB },
  ],
  recurring: { interval: 'month', usage_type: 'licensed' },
  product_data: { name: 'Backup Buddies Storage' },
});

console.log('\nDone. Add this to .env on app-1:\n');
console.log(`STRIPE_STORAGE_PRICE_ID=${price.id}`);
console.log(`\n(Product: ${price.product}, Price: ${price.id})`);
