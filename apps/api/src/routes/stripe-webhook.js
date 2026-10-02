import { Router } from 'express';
import { pool } from '../db/pool.js';
import { stripe } from '../lib/stripe.js';

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

      case 'checkout.session.completed': {
        const session = event.data.object;
        // client_reference_id is set to the account's id when we create
        // the Checkout session in routes/billing.js — that's how we tie
        // "a subscription now exists" back to a specific account.
        if (session.mode === 'subscription' && session.subscription && session.client_reference_id) {
          const subscription = await stripe.subscriptions.retrieve(session.subscription);
          // The subscription may carry a second item now (relay bandwidth,
          // see billing.js's checkout) alongside storage — find the storage
          // one by price id rather than assuming index 0, since
          // stripe_subscription_item_id is specifically the one
          // syncSubscriptionQuantity() (billing.js) keeps in sync with
          // pledged GB. Falls back to the first item for safety if the
          // price id isn't configured/found.
          const storageItem =
            subscription.items.data.find(
              (item) => item.price?.id === process.env.STRIPE_STORAGE_PRICE_ID,
            ) ?? subscription.items.data[0];
          await pool.query(
            `UPDATE accounts
                SET stripe_subscription_id = $1,
                    stripe_subscription_item_id = $2,
                    relay_status = 'active'
              WHERE id = $3`,
            [subscription.id, storageItem?.id ?? null, session.client_reference_id],
          );
        }
        break;
      }

      case 'customer.subscription.deleted': {
        const subscription = event.data.object;
        // Cancellation (self-service via the billing portal, or a dunning
        // process giving up after repeated failures) — clear the stored
        // subscription so the account can start a fresh one later.
        await pool.query(
          `UPDATE accounts
              SET stripe_subscription_id = NULL,
                  stripe_subscription_item_id = NULL
            WHERE stripe_subscription_id = $1`,
          [subscription.id],
        );
        break;
      }

      case 'customer.subscription.updated': {
        const subscription = event.data.object;
        console.log('Subscription updated', subscription.id, subscription.status);
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
