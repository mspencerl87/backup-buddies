import express from 'express';
import { healthRouter } from './routes/health.js';
import { stripeWebhookRouter } from './routes/stripe-webhook.js';

const app = express();

// Stripe's webhook route needs the raw request body (not JSON-parsed) for
// signature verification — scope express.raw() to just this path, and
// mount it before the global express.json() parser below would otherwise
// consume the body first.
app.use('/webhooks/stripe', express.raw({ type: 'application/json' }));
app.use(stripeWebhookRouter);

app.use(express.json());
app.use(healthRouter);

// TODO: routes for account signup, buddy pairing, device registration,
// and quota lookups go here as the product grows past this skeleton.

const port = process.env.PORT || 3000;
app.listen(port, () => {
  console.log(`backup-buddies api listening on :${port}`);
});
