import express from 'express';
import 'express-async-errors'; // must load before any router is defined — patches Express 4's router so a rejected promise in an async handler reaches the error-handling middleware below instead of hanging the request
import cookieParser from 'cookie-parser';
import { healthRouter } from './routes/health.js';
import { stripeWebhookRouter } from './routes/stripe-webhook.js';
import { authRouter } from './routes/auth.js';
import { meRouter } from './routes/me.js';
import { invitesRouter } from './routes/invites.js';
import { billingRouter } from './routes/billing.js';
import { devicesRouter } from './routes/devices.js';

const app = express();

// Stripe's webhook route needs the raw request body (not JSON-parsed) for
// signature verification — scope express.raw() to just this path, and
// mount it before the global express.json() parser below would otherwise
// consume the body first.
app.use('/webhooks/stripe', express.raw({ type: 'application/json' }));
app.use(stripeWebhookRouter);

app.use(express.json());
app.use(cookieParser());

app.use(healthRouter);
app.use(authRouter);
app.use(meRouter);
app.use(invitesRouter);
app.use(billingRouter);
app.use(devicesRouter);

// Catch-all error handler: any route above that throws (or rejects, since
// Express 4 doesn't auto-catch async errors) lands here instead of
// crashing the process or hanging the request.
app.use((err, req, res, next) => {
  console.error(err);
  res.status(500).json({ error: 'internal server error' });
});

const port = process.env.PORT || 3000;
app.listen(port, () => {
  console.log(`backup-buddies api listening on :${port}`);
});
