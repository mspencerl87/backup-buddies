import { Router } from 'express';
import { pool } from '../db/pool.js';
import { stripe } from '../lib/stripe.js';
import { sendMail, SUPPORT_EMAIL } from '../lib/mailer.js';

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
        // Clears a pause from a previous failed invoice (either the retry
        // that follows it succeeded, or the person fixed their card and a
        // later invoice went through) — relay_paused_at/reason go back to
        // NULL so the account doesn't carry a stale "paused since" date
        // into the admin view once it's actually active again.
        await pool.query(
          `UPDATE accounts
             SET relay_status = 'active',
                 relay_paused_at = NULL,
                 relay_paused_reason = NULL
           WHERE stripe_customer_id = $1`,
          [invoice.customer],
        );
        break;
      }

      case 'invoice.payment_failed': {
        const invoice = event.data.object;
        // Stripe retries a failed invoice automatically (its Smart
        // Retries schedule) and sends this same event on each attempt.
        // next_payment_attempt is set to the next retry's timestamp until
        // retries are exhausted, when it comes through as null — that's
        // the signal to actually pause, rather than reacting to the first
        // failure, which could just be a card temporarily declined for an
        // unrelated reason (e.g. the bank's fraud check) that clears
        // itself on retry.
        if (invoice.next_payment_attempt !== null) break;

        const result = await pool.query(
          `UPDATE accounts
             SET relay_status = 'paused',
                 relay_paused_at = now(),
                 relay_paused_reason = 'payment_failed'
           WHERE stripe_customer_id = $1 AND relay_status <> 'paused'
           RETURNING id, email`,
          [invoice.customer],
        );

        // RETURNING came back empty if the account was already paused
        // (e.g. a duplicate webhook delivery, which Stripe does send on
        // occasion) — nothing changed, so there's no reason to re-send
        // the email.
        if (result.rows.length > 0) {
          const account = result.rows[0];
          const appUrl = process.env.APP_URL || '';
          try {
            await sendMail({
              to: account.email,
              subject: 'Backup Buddies: payment failed — your buddy syncing is paused',
              text:
                `We weren't able to charge your card after several attempts, so your buddy syncing ` +
                `has been paused — your existing backups are untouched, but your device won't send or ` +
                `receive any new files with your buddy until this is fixed.\n\n` +
                `Update your payment method here: ${appUrl}/dashboard.html#billing\n\n` +
                `Syncing resumes automatically as soon as a payment goes through.\n\n` +
                `Questions, or think this is a mistake? Just reply to this email (it goes to ${SUPPORT_EMAIL}), or use Contact support on your dashboard.`,
              replyTo: SUPPORT_EMAIL,
              html:
                `<p>We weren't able to charge your card after several attempts, so your buddy syncing ` +
                `has been paused — your existing backups are untouched, but your device won't send or ` +
                `receive any new files with your buddy until this is fixed.</p>` +
                `<p><a href="${appUrl}/dashboard.html#billing">Update your payment method</a></p>` +
                `<p>Syncing resumes automatically as soon as a payment goes through.</p>` +
                `<p>Questions, or think this is a mistake? Email <a href="mailto:${SUPPORT_EMAIL}">${SUPPORT_EMAIL}</a> ` +
                `or use <a href="${appUrl}/dashboard.html#support">Contact support</a> on your dashboard.</p>`,
            });
          } catch (err) {
            // Don't fail the whole webhook over a flaky SMTP send — the
            // account is already correctly paused in the DB (the thing
            // that actually matters for stopping fees), and the admin
            // view surfaces it either way.
            console.error(`Failed to send payment-failed email to ${account.email}:`, err);
          }
        }
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
                    relay_status = 'active',
                    relay_paused_at = NULL,
                    relay_paused_reason = NULL
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
