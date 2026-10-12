#!/usr/bin/env node
// One-shot setup script: creates the Stripe Billing Meter + metered Price
// for relay-bandwidth billing ($0.02/GB — relayed bytes only; direct
// peer-to-peer transfers never touch the relay and aren't metered at all).
// Prints both ids to put into .env.
//
// Run with STRIPE_SECRET_KEY set in your shell — no npm install needed,
// this talks to Stripe's REST API directly over fetch (built into Node 20+):
//
//   STRIPE_SECRET_KEY=sk_test_xxx node scripts/create-stripe-relay-meter-and-price.mjs
//
// Safe to run more than once — each run creates a NEW Meter/Product/Price
// (Stripe Meters and Prices are immutable once created), so if you re-run
// this to fix something, update STRIPE_RELAY_PRICE_ID to the newest Price id
// it prints (and EVENT_NAME below plus
// apps/api/src/lib/relay-billing.js's RELAY_METER_EVENT_NAME, if you also
// change the event name — they must match exactly).
//
// Once this is created, existing paying accounts (whose subscriptions
// predate relay billing) need the new Price added separately — see
// scripts/add-relay-price-to-existing-subscriptions.mjs.

const secretKey = process.env.STRIPE_SECRET_KEY;
if (!secretKey) {
  console.error('Set STRIPE_SECRET_KEY in your shell before running this script.');
  process.exit(1);
}

// Rate rationale: the VPS host behind relay.filegarden.net charges roughly
// $0.005-0.01/GB for network egress overage as of 2026 (typical for VPS
// providers with a monthly transfer allowance). $0.02/GB keeps a
// healthy margin (2-4x) over that actual cost while staying far below
// typical cloud egress pricing (AWS/GCP commonly run $0.08-0.12/GB) and is
// easy to explain to users.
//
// Reported to Stripe in whole megabytes, not bytes or fractional GB, so the
// per-unit price doesn't need a many-decimal-place amount.
const USD_PER_GB = 0.02;
const CENTS_PER_MB = ((USD_PER_GB * 100) / 1024).toFixed(6); // "0.001953"

// Must match RELAY_METER_EVENT_NAME in apps/api/src/lib/relay-billing.js —
// that's the string the API sends when reporting usage. Stripe's
// meter-events API is addressed by this name, not the meter's id.
const EVENT_NAME = 'relay_bandwidth_mb';

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

console.log(`Creating a Billing Meter ("${EVENT_NAME}")...`);
const meter = await stripeRequest('billing/meters', {
  display_name: 'Relay bandwidth (MB)',
  event_name: EVENT_NAME,
  default_aggregation: { formula: 'sum' },
  value_settings: { event_payload_key: 'value' },
  customer_mapping: { type: 'by_id', event_payload_key: 'stripe_customer_id' },
});

console.log(`Creating a metered Price ($${USD_PER_GB}/GB = ${CENTS_PER_MB}¢/MB)...`);
const price = await stripeRequest('prices', {
  currency: 'usd',
  billing_scheme: 'per_unit',
  unit_amount_decimal: CENTS_PER_MB,
  recurring: { interval: 'month', usage_type: 'metered', meter: meter.id },
  product_data: { name: 'Backup Buddies Relay Bandwidth' },
});

console.log('\nDone. Add these to .env on app-1:\n');
console.log(`STRIPE_RELAY_PRICE_ID=${price.id}`);
console.log(`STRIPE_RELAY_METER_ID=${meter.id}`);
console.log(`\n(Meter: ${meter.id}, Price: ${price.id}, Product: ${price.product})`);
console.log(
  '\nNext: if there are existing paying accounts, run scripts/add-relay-price-to-existing-subscriptions.mjs',
);
