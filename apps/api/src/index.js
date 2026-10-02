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
import { adminRouter } from './routes/admin.js';
import { syncRelayUsageToStripe } from './jobs/sync-relay-usage.js';
import { runStaleAccountCleanup } from './jobs/stale-account-cleanup.js';
import { sendStaleAccountDigest } from './jobs/stale-account-digest.js';

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
app.use(adminRouter);

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

// Periodically reports accumulated relay_usage_events to Stripe (see
// jobs/sync-relay-usage.js). Gated on STRIPE_RELAY_PRICE_ID so this is a
// no-op until relay billing is actually set up on a given deployment —
// recordRelayUsage() (lib/relay-billing.js) still writes rows regardless,
// so turning this on later bills whatever accumulated in the meantime
// rather than losing it.
if (process.env.STRIPE_RELAY_PRICE_ID) {
  const SYNC_INTERVAL_MS = 15 * 60 * 1000; // 15 minutes
  setInterval(() => {
    syncRelayUsageToStripe().catch((err) => {
      console.error('Relay usage sync to Stripe failed', err);
    });
  }, SYNC_INTERVAL_MS);
  // Don't wait a full interval after a fresh deploy/restart before the
  // first sync.
  setTimeout(() => {
    syncRelayUsageToStripe().catch((err) => {
      console.error('Relay usage sync to Stripe failed', err);
    });
  }, 60_000);
}

// Stale-account lifecycle (jobs/stale-account-cleanup.js,
// jobs/stale-account-digest.js): an inactivity warning email daily, and a
// digest to the admin(s) of who's gone quiet since that warning — deletion
// is always a deliberate human action from /admin.html, never automatic.
// Opt-in and off by default (STALE_ACCOUNT_CLEANUP_ENABLED=true) so this
// doesn't start emailing real accounts the moment SMTP happens to be
// configured for something else.
if (process.env.STALE_ACCOUNT_CLEANUP_ENABLED === 'true') {
  const DAY_MS = 24 * 60 * 60 * 1000;

  setInterval(() => {
    runStaleAccountCleanup()
      .then(({ cleared, warned }) => {
        if (cleared > 0 || warned > 0) {
          console.log(`Stale-account cleanup: cleared ${cleared} warning(s), sent ${warned} new one(s).`);
        }
      })
      .catch((err) => console.error('Stale-account cleanup failed', err));
  }, DAY_MS);
  setTimeout(() => {
    runStaleAccountCleanup().catch((err) => console.error('Stale-account cleanup failed', err));
  }, 5 * 60 * 1000); // 5 minutes after boot, not immediately on every restart

  // Checked daily (not every 30 days) but only actually sends once a
  // month — sendStaleAccountDigest() tracks its own last-send time in the
  // job_runs table (migration 008), not in memory, so a restart/redeploy
  // can't cause it to fire early or skip a cycle the way a bare
  // setInterval(30 * DAY_MS) would.
  setInterval(() => {
    sendStaleAccountDigest()
      .then(({ candidateCount, sent }) => {
        if (sent) console.log(`Stale-account digest sent: ${candidateCount} candidate(s).`);
      })
      .catch((err) => console.error('Stale-account digest failed', err));
  }, DAY_MS);
  setTimeout(() => {
    sendStaleAccountDigest().catch((err) => console.error('Stale-account digest failed', err));
  }, 10 * 60 * 1000); // staggered from the cleanup job's own startup run
}
