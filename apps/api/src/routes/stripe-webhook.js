import { Router } from 'express';
import Stripe from 'stripe';
import { pool } from '../db/pool.js';

const stripe = new Stripe(process.env.STRIPE_SECRET_KEY);

export const stripeWebhookRouter = Router();

// IMPORTANT: this route must receive the raw request body (not JSON-parsed)
// for Stripe's signature verification to work — see how this router is
// mounted in src/index.js (express.raw(), not express.json()).
stripeWebhookRouter.post('/webhooks/stripe', async (req, res) => {
  const signature = req.headers['stripe-signature'];

  let event;
  try {
    event = stripe.webhooks.constructEvent(
      req.body,
      signature,
      process.env.STRIPE_WEBHOOK_SECRET,
    );
  } catch (err) {
    console.error('Stripe webhook signature verification failed', err.message);
    return res.status(400).send(`Webhook Error: ${err.message}`);
  }

  try {
    switch (event.type) {
      case 'invoice.payment_succeeded': {
        const invoice = event.data.object;
        await pool.query(
          `UPDATE accounts
             SET relay_status = 'active'
           WHERE stripe_customer_id = $1`,
          [invoice.customer],
        );
        break;
      }

      case 'invoice.payment_failed': {
        const invoice = event.data.object;
        // TODO: decide on a grace period before pausing relay access —
        // flagging immediately on the first failed charge may be too harsh.
        await pool.query(
          `UPDATE accounts
             SET relay_status = 'paused'
           WHERE stripe_customer_id = $1`,
          [invoice.customer],
        );
        break;
      }

      case 'customer.subscription.updated': {
        const subscription = event.data.object;
        // TODO: map subscription.items back to a tier name and update
        // accounts.tier, once the Price IDs for each tier are created in
        // the Stripe dashboard.
        console.log('Subscription updated', subscription.id);
        break;
      }

      default:
        // Unhandled event types are fine to ignore — Stripe sends many
        // event types this app doesn't need to react to.
        break;
    }

    res.json({ received: true });
  } catch (err) {
    // Returning 500 tells Stripe to retry the webhook later, which is the
    // right behavior for a transient DB error.
    console.error('Error handling Stripe webhook', err);
    res.status(500).send('Webhook handler error');
  }
});
