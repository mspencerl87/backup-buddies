import Stripe from 'stripe';

// Shared Stripe client — used by the webhook handler and the billing
// routes alike, so there's exactly one place reading STRIPE_SECRET_KEY.
export const stripe = new Stripe(process.env.STRIPE_SECRET_KEY);
