import { Router } from 'express';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';
import { requireAdmin } from '../middleware/require-admin.js';
import { syncSubscriptionQuantity, pledgedGBForAccount, FREE_GB } from './billing.js';
import { isAdminEmail } from '../lib/admin.js';
import { stripe } from '../lib/stripe.js';
import { FREE_RELAY_GB_PER_MONTH, FREE_RELAY_CAP_REASON } from '../lib/relay-billing.js';
import { sendVerificationEmail } from '../lib/email-verification.js';
import { createPasswordResetLink } from '../lib/password-reset.js';
import { isMailConfigured } from '../lib/mailer.js';

export const adminRouter = Router();

// Every route here is both session-authed AND restricted to ADMIN_EMAILS
// (see lib/admin.js) — requireAuth first so req.account exists for
// requireAdmin to check, and so an unauthenticated caller gets a plain 401
// rather than a 403 that would confirm admin routes exist at all.
adminRouter.use(requireAuth, requireAdmin);

// All accounts, with cheap counts and a buddy summary for a quick scan —
// not the full detail (devices/pairings/invites), which is a separate
// per-account fetch below so this list stays fast regardless of how many
// accounts exist. The buddies array only carries pledged_bytes (what's
// agreed over the API) — there's no "used" figure to show here, since
// actual usage is only ever known locally on each client's own dashboard
// and isn't reported back to this server.
adminRouter.get('/admin/accounts', async (req, res) => {
  const result = await pool.query(
    `SELECT accounts.id, accounts.email, accounts.nickname, accounts.tier,
            accounts.relay_status, accounts.relay_paused_at, accounts.relay_paused_reason,
            accounts.created_at, accounts.billing_exempt,
            (accounts.stripe_subscription_id IS NOT NULL) AS has_subscription,
            (SELECT count(*) FROM devices
              WHERE devices.account_id = accounts.id) AS device_count,
            COALESCE(buddies.buddies, '[]'::json) AS buddies
       FROM accounts
       LEFT JOIN LATERAL (
         SELECT json_agg(
                  json_build_object(
                    'email', other.email,
                    'pledged_bytes', buddy_pairings.pledged_bytes,
                    'status', buddy_pairings.status
                  ) ORDER BY other.email
                ) AS buddies
           FROM buddy_pairings
           JOIN accounts other
             ON other.id = CASE WHEN buddy_pairings.account_a_id = accounts.id
                                 THEN buddy_pairings.account_b_id
                                 ELSE buddy_pairings.account_a_id END
          WHERE buddy_pairings.account_a_id = accounts.id
             OR buddy_pairings.account_b_id = accounts.id
       ) buddies ON true
      ORDER BY accounts.created_at DESC`,
  );
  // is_admin comes from ADMIN_EMAILS (lib/admin.js), not a database column
  // — same "allowlist, not a role" model /me already uses — computed here
  // rather than in SQL so the admin UI can give these accounts their own
  // column instead of lumping them into free/paid like everyone else.
  const accounts = result.rows.map((row) => ({ ...row, is_admin: isAdminEmail(row.email) }));
  res.json({ accounts });
});

// Service-wide totals for the stats panel at the top of the admin page.
// Computed at most twice a day (cached in memory for STATS_TTL_MS) since
// nobody needs these to the minute; ?refresh=1 recomputes on demand. All
// plain aggregates, a few milliseconds each at this size. Relay figures are
// what clients report on their 30s poll (see lib/relay-billing.js), not
// counted by the relay itself, and nothing is recorded while an account is
// paused. Stored bytes aren't here: only clients know them.
const STATS_TTL_MS = 12 * 60 * 60 * 1000;
let statsCache = null;

async function computeStats() {
  const [relay, accounts, devices, versions, pairings, listings] = await Promise.all([
    // "Paid" = accounts subscribed right now, so last month's usage from
    // someone who has since cancelled counts as free here.
    pool.query(
      `SELECT COALESCE(SUM(e.bytes_relayed), 0) AS all_time,
              COALESCE(SUM(e.bytes_relayed) FILTER (WHERE e.occurred_at >= m.start), 0) AS this_month,
              COALESCE(SUM(e.bytes_relayed) FILTER (
                WHERE e.occurred_at >= m.start AND a.stripe_subscription_id IS NOT NULL
              ), 0) AS this_month_paid,
              COUNT(DISTINCT e.account_id) FILTER (WHERE e.occurred_at >= m.start) AS accounts_this_month
         FROM relay_usage_events e
         LEFT JOIN accounts a ON a.id = e.account_id
        CROSS JOIN (SELECT date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS start) m`,
    ),
    pool.query(
      `SELECT count(*) AS total,
              count(*) FILTER (WHERE email_verified) AS verified,
              count(*) FILTER (WHERE stripe_subscription_id IS NOT NULL) AS subscribed,
              count(*) FILTER (WHERE relay_status <> 'active') AS paused,
              count(*) FILTER (WHERE created_at >= now() - interval '7 days') AS new_7d,
              count(*) FILTER (WHERE created_at >= now() - interval '30 days') AS new_30d
         FROM accounts`,
    ),
    pool.query(
      `SELECT count(*) AS total,
              count(*) FILTER (WHERE last_seen_at >= now() - interval '5 minutes') AS online_now,
              count(*) FILTER (WHERE last_seen_at >= now() - interval '24 hours') AS seen_24h
         FROM devices`,
    ),
    pool.query(
      `SELECT COALESCE(client_version, 'unknown') AS version, count(*) AS devices
         FROM devices
        GROUP BY 1
        ORDER BY 2 DESC, 1 DESC`,
    ),
    pool.query(
      `SELECT count(*) AS active, COALESCE(SUM(pledged_bytes), 0) AS pledged_bytes
         FROM buddy_pairings
        WHERE status = 'active'`,
    ),
    pool.query(
      `SELECT count(*) AS open_count FROM invites WHERE is_public AND status = 'pending'`,
    ),
  ]);

  const n = (v) => Number(v);
  const r = relay.rows[0];
  const a = accounts.rows[0];
  const d = devices.rows[0];
  const p = pairings.rows[0];
  return {
    computed_at: new Date().toISOString(),
    relay: {
      all_time_bytes: n(r.all_time),
      this_month_bytes: n(r.this_month),
      this_month_paid_bytes: n(r.this_month_paid),
      accounts_this_month: n(r.accounts_this_month),
    },
    accounts: {
      total: n(a.total),
      verified: n(a.verified),
      subscribed: n(a.subscribed),
      paused: n(a.paused),
      new_7d: n(a.new_7d),
      new_30d: n(a.new_30d),
    },
    devices: {
      total: n(d.total),
      online_now: n(d.online_now),
      seen_24h: n(d.seen_24h),
      versions: versions.rows.map((row) => ({ version: row.version, devices: n(row.devices) })),
    },
    pairings: { active: n(p.active), pledged_bytes: n(p.pledged_bytes) },
    marketplace: { open_listings: n(listings.rows[0].open_count) },
  };
}

adminRouter.get('/admin/stats', async (req, res) => {
  const fresh = statsCache && Date.now() - Date.parse(statsCache.computed_at) < STATS_TTL_MS;
  if (!fresh || req.query.refresh === '1') {
    statsCache = await computeStats();
  }
  res.json(statsCache);
});

// One account's full detail — everything you'd need on a support ticket:
// its devices (to spot a stuck/misregistered one), its buddy pairings
// (with the other side's email, not just an id), and the invites it has
// created. Deliberately leaves out password_hash and token_hash — nothing
// here needs them, and there's no reason to ever pull them over the wire.
adminRouter.get('/admin/accounts/:id', async (req, res) => {
  const accountId = req.params.id;

  const [
    accountResult,
    devicesResult,
    pairingsResult,
    invitesResult,
    redeemedResult,
    sessionsResult,
    relayResult,
    pledgedGB,
  ] = await Promise.all([
    pool.query(
      `SELECT id, email, nickname, tier, relay_status, relay_paused_at, relay_paused_reason,
              created_at, terms_accepted_at, billing_exempt,
              stripe_customer_id, stripe_subscription_id, stripe_subscription_item_id,
              (password_hash IS NOT NULL AND password_hash <> '') AS has_password,
              (google_sub IS NOT NULL) AS has_google,
              email_verified, email_verification_sent_at,
              stale_warning_sent_at
         FROM accounts
        WHERE id = $1`,
      [accountId],
    ),
    pool.query(
      `SELECT id, iroh_node_id, label, token_last_four, created_at, last_seen_at,
              to_jsonb(devices) ->> 'client_version' AS client_version
         FROM devices
        WHERE account_id = $1
        ORDER BY created_at`,
      [accountId],
    ),
    pool.query(
      `SELECT buddy_pairings.id, buddy_pairings.account_a_id, buddy_pairings.account_b_id,
              buddy_pairings.pledged_bytes, buddy_pairings.status, buddy_pairings.created_at,
              other.email AS other_email
         FROM buddy_pairings
         JOIN accounts other
           ON other.id = CASE WHEN buddy_pairings.account_a_id = $1
                               THEN buddy_pairings.account_b_id
                               ELSE buddy_pairings.account_a_id END
        WHERE buddy_pairings.account_a_id = $1 OR buddy_pairings.account_b_id = $1
        ORDER BY buddy_pairings.created_at DESC`,
      [accountId],
    ),
    pool.query(
      `SELECT id, code, pledged_bytes, status, is_public, note, created_at,
              redeemed_by_account_id, redeemed_at
         FROM invites
        WHERE inviter_account_id = $1
        ORDER BY created_at DESC`,
      [accountId],
    ),
    // Invites this account *redeemed* — i.e. how it got paired in the
    // first place, and with whom. The "Invites created" list above only
    // covers the other direction.
    pool.query(
      `SELECT invites.id, invites.code, invites.pledged_bytes, invites.redeemed_at,
              inviter.email AS inviter_email
         FROM invites
         LEFT JOIN accounts inviter ON inviter.id = invites.inviter_account_id
        WHERE invites.redeemed_by_account_id = $1
        ORDER BY invites.redeemed_at DESC NULLS LAST`,
      [accountId],
    ),
    // Sessions are created on every login (password or Google), so the
    // newest one is effectively "last signed in".
    pool.query(
      `SELECT count(*) FILTER (WHERE expires_at > now()) AS active_sessions,
              max(created_at) AS last_login_at
         FROM sessions
        WHERE account_id = $1`,
      [accountId],
    ),
    // Relay usage is only ever recorded for subscribed, unpaused accounts
    // (lib/relay-billing.js), so all zeroes here is normal for a free one.
    // unsynced = recorded locally but not yet reported to Stripe by
    // jobs/sync-relay-usage.js — a growing unsynced pile means that job
    // is failing for this account.
    pool.query(
      `SELECT COALESCE(sum(bytes_relayed), 0) AS total_bytes,
              COALESCE(sum(bytes_relayed) FILTER (WHERE occurred_at > now() - interval '30 days'), 0)
                AS last_30d_bytes,
              COALESCE(sum(bytes_relayed) FILTER (
                WHERE occurred_at >= date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
              ), 0) AS this_month_bytes,
              COALESCE(sum(bytes_relayed) FILTER (WHERE NOT synced_to_stripe), 0) AS unsynced_bytes,
              count(*) FILTER (WHERE NOT synced_to_stripe) AS unsynced_events,
              max(occurred_at) AS last_event_at
         FROM relay_usage_events
        WHERE account_id = $1`,
      [accountId],
    ),
    pledgedGBForAccount(accountId),
  ]);

  if (accountResult.rows.length === 0) {
    return res.status(404).json({ error: 'account not found' });
  }

  const sessions = sessionsResult.rows[0];
  const relay = relayResult.rows[0];

  res.json({
    account: { ...accountResult.rows[0], is_admin: isAdminEmail(accountResult.rows[0].email) },
    devices: devicesResult.rows,
    pairings: pairingsResult.rows,
    invites: invitesResult.rows,
    redeemed_invites: redeemedResult.rows,
    activity: {
      active_sessions: Number(sessions.active_sessions),
      last_login_at: sessions.last_login_at,
    },
    pledge: {
      pledged_gb: pledgedGB,
      free_allowance_gb: FREE_GB,
      billable_gb: Math.max(0, pledgedGB - FREE_GB),
    },
    relay_usage: {
      total_bytes: Number(relay.total_bytes),
      last_30d_bytes: Number(relay.last_30d_bytes),
      this_month_bytes: Number(relay.this_month_bytes),
      // Only enforced for free accounts (lib/relay-billing.js).
      free_allowance_gb: FREE_RELAY_GB_PER_MONTH,
      unsynced_bytes: Number(relay.unsynced_bytes),
      unsynced_events: Number(relay.unsynced_events),
      last_event_at: relay.last_event_at,
    },
  });
});

// --- Troubleshooting actions ---
//
// Each of these is a single, targeted fix for a common support case, so an
// admin never has to drop into psql for it. They're logged to stdout (the
// API container's logs) with the acting admin's email — not a full audit
// table yet, but enough to answer "who did this?" after the fact.

function logAdminAction(req, action, extra = '') {
  console.log(
    `[admin] ${req.account.email} ${action} account=${req.params.id}${extra ? ` ${extra}` : ''}`,
  );
}

async function loadAccountOr404(req, res, columns = 'id, email') {
  const result = await pool.query(`SELECT ${columns} FROM accounts WHERE id = $1`, [req.params.id]);
  if (result.rows.length === 0) {
    res.status(404).json({ error: 'account not found' });
    return null;
  }
  return result.rows[0];
}

// Emails a single-use link to set a new password (lib/password-reset.js).
// Works for Google-only accounts too — the link just adds a password.
// The link is always returned so it can be pasted into a support reply if
// email isn't configured or the user can't find it; it's single-use and
// expires, and issuing a new one invalidates the previous one.
adminRouter.post('/admin/accounts/:id/password-reset', async (req, res) => {
  const account = await loadAccountOr404(req, res);
  if (!account) return;

  const result = await createPasswordResetLink(account);
  logAdminAction(req, 'password-reset', `emailed=${result.emailed}`);
  res.json(result);
});

// Marks the email verified without the user clicking their link — for when
// the verification email never arrives (spam filter, typo'd SMTP config…)
// and the admin has confirmed ownership some other way (e.g. they wrote in
// from that address).
adminRouter.post('/admin/accounts/:id/verify-email', async (req, res) => {
  const result = await pool.query(
    `UPDATE accounts
        SET email_verified = true,
            email_verification_token_hash = NULL
      WHERE id = $1
      RETURNING id`,
    [req.params.id],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'account not found' });
  }
  logAdminAction(req, 'verify-email');
  res.json({ ok: true });
});

// Same as the user's own "resend" button, cooldown included.
adminRouter.post('/admin/accounts/:id/resend-verification', async (req, res) => {
  const account = await loadAccountOr404(
    req,
    res,
    'id, email, email_verified, email_verification_sent_at',
  );
  if (!account) return;

  if (!isMailConfigured()) {
    return res.status(503).json({
      error: "Email isn't configured on this server (SMTP_* env vars), so nothing can be sent — use \"Mark email verified\" instead.",
    });
  }
  const result = await sendVerificationEmail(account);
  if (result === 'already_verified') {
    return res.status(409).json({ error: 'This account is already verified.' });
  }
  if (result === 'cooldown') {
    return res
      .status(429)
      .json({ error: 'A verification email was sent less than a minute ago — wait before resending.' });
  }
  logAdminAction(req, 'resend-verification');
  res.json({ ok: true });
});

// Logs the account out everywhere (every browser session). Doesn't touch
// device tokens — those are separate credentials; delete a device to
// revoke one of those.
adminRouter.post('/admin/accounts/:id/revoke-sessions', async (req, res) => {
  if (req.params.id === req.account.id) {
    return res.status(400).json({ error: "can't sign yourself out everywhere from here" });
  }
  const result = await pool.query('DELETE FROM sessions WHERE account_id = $1', [req.params.id]);
  logAdminAction(req, 'revoke-sessions', `count=${result.rowCount}`);
  res.json({ ok: true, revoked: result.rowCount });
});

// Manually pause or resume relay syncing. Pausing records reason 'admin'
// so it's distinguishable from a payment-failed pause. Resuming clears the
// pause regardless of why it happened — note that a later
// invoice.payment_failed webhook can pause it again, and a payment-failed
// account resumed here can accrue relay fees again (relay-billing.js).
adminRouter.post('/admin/accounts/:id/relay', async (req, res) => {
  const { action, reason } = req.body ?? {};
  if (action !== 'pause' && action !== 'resume') {
    return res.status(400).json({ error: "action must be 'pause' or 'resume'" });
  }

  const result =
    action === 'pause'
      ? await pool.query(
          `UPDATE accounts
              SET relay_status = 'paused',
                  relay_paused_at = COALESCE(relay_paused_at, now()),
                  relay_paused_reason = $2
            WHERE id = $1
            RETURNING id`,
          [req.params.id, typeof reason === 'string' && reason.trim() ? reason.trim().slice(0, 100) : 'admin'],
        )
      : await pool.query(
          `UPDATE accounts
              SET relay_status = 'active',
                  relay_paused_at = NULL,
                  relay_paused_reason = NULL
            WHERE id = $1
            RETURNING id`,
          [req.params.id],
        );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'account not found' });
  }
  logAdminAction(req, `relay-${action}`);
  res.json({ ok: true });
});

// Complimentary account: never needs a card. Skips the free-allowance 402s
// in invites.js, and relay usage is never billed or capped
// (lib/relay-billing.js). Manual only; ADMIN_EMAILS doesn't imply it.
// Doesn't touch Stripe: a live subscription keeps billing for storage
// until it's cancelled (Stripe section below).
//
// Granting it also lifts a free-allowance pause, since that cap no longer
// applies; any other pause (payment_failed, admin) is left alone. Revoking
// it doesn't re-pause anything: the next relay report re-applies the cap.
adminRouter.post('/admin/accounts/:id/billing-exempt', async (req, res) => {
  const { exempt } = req.body ?? {};
  if (typeof exempt !== 'boolean') {
    return res.status(400).json({ error: 'exempt must be true or false' });
  }

  const result = await pool.query(
    `UPDATE accounts
        SET billing_exempt = $2,
            relay_status = CASE WHEN $2 AND relay_paused_reason = $3 THEN 'active' ELSE relay_status END,
            relay_paused_at = CASE WHEN $2 AND relay_paused_reason = $3 THEN NULL ELSE relay_paused_at END,
            relay_paused_reason = CASE WHEN $2 AND relay_paused_reason = $3 THEN NULL ELSE relay_paused_reason END
      WHERE id = $1
      RETURNING id`,
    [req.params.id, exempt, FREE_RELAY_CAP_REASON],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'account not found' });
  }
  logAdminAction(req, 'billing-exempt', `exempt=${exempt}`);
  res.json({ ok: true, billing_exempt: exempt });
});

// --- Stripe ---

function stripeConfigured() {
  return Boolean(process.env.STRIPE_SECRET_KEY);
}

function stripeDashboardUrl(livemode, path) {
  return `https://dashboard.stripe.com/${livemode ? '' : 'test/'}${path}`;
}

// Subscriptions that are still billing (or trying to). 'incomplete' is left
// out — that's an abandoned checkout, not a live subscription.
const LIVE_SUBSCRIPTION_STATUSES = new Set(['active', 'trialing', 'past_due', 'unpaid']);

function summarizeSubscription(sub, livemode) {
  // API 2024-06-20 (stripe-node 16) has the period on the subscription;
  // newer API versions moved it to the items. Read either.
  const firstItem = sub.items?.data?.[0];
  return {
    id: sub.id,
    status: sub.status,
    created: sub.created,
    current_period_start: sub.current_period_start ?? firstItem?.current_period_start ?? null,
    current_period_end: sub.current_period_end ?? firstItem?.current_period_end ?? null,
    cancel_at_period_end: sub.cancel_at_period_end,
    cancel_at: sub.cancel_at,
    canceled_at: sub.canceled_at,
    ended_at: sub.ended_at,
    items: (sub.items?.data || []).map((item) => ({
      id: item.id,
      price_id: item.price?.id,
      nickname: item.price?.nickname,
      product: typeof item.price?.product === 'string' ? item.price.product : item.price?.product?.id,
      quantity: item.quantity ?? null,
      usage_type: item.price?.recurring?.usage_type ?? null,
      billing_scheme: item.price?.billing_scheme ?? null,
      unit_amount: item.price?.unit_amount ?? null,
      currency: item.price?.currency,
      interval: item.price?.recurring?.interval ?? null,
      is_storage: item.price?.id === process.env.STRIPE_STORAGE_PRICE_ID,
      is_relay: item.price?.id === process.env.STRIPE_RELAY_PRICE_ID,
    })),
    dashboard_url: stripeDashboardUrl(livemode, `subscriptions/${sub.id}`),
  };
}

function summarizePaymentMethod(pm) {
  if (!pm || typeof pm === 'string') return null;
  if (pm.card) {
    return {
      type: 'card',
      brand: pm.card.brand,
      last4: pm.card.last4,
      exp_month: pm.card.exp_month,
      exp_year: pm.card.exp_year,
    };
  }
  return { type: pm.type };
}

// Pulls this account's live Stripe state so billing questions can be
// answered without leaving the admin page, plus a list of consistency
// checks between what Stripe says and what our database says — the usual
// cause of "I'm being billed wrong" is a webhook that never landed.
// Separate from GET /admin/accounts/:id so a slow or failing Stripe call
// never holds up the rest of the detail view.
adminRouter.get('/admin/accounts/:id/stripe', async (req, res) => {
  const account = await loadAccountOr404(
    req,
    res,
    'id, email, stripe_customer_id, stripe_subscription_id, stripe_subscription_item_id, relay_status',
  );
  if (!account) return;

  if (!stripeConfigured()) {
    return res.json({ configured: false });
  }

  const checks = [];
  const db = {
    customer_id: account.stripe_customer_id,
    subscription_id: account.stripe_subscription_id,
    subscription_item_id: account.stripe_subscription_item_id,
  };

  // No customer on file: the account never started checkout — but look
  // for a customer under the same email in case the link was lost.
  if (!account.stripe_customer_id) {
    const found = await stripe.customers.list({ email: account.email, limit: 5 });
    const candidates = found.data.map((c) => ({
      id: c.id,
      created: c.created,
      dashboard_url: stripeDashboardUrl(c.livemode, `customers/${c.id}`),
    }));
    if (candidates.length) {
      checks.push({
        level: 'warn',
        message:
          `No Stripe customer is linked in our database, but ${candidates.length} customer(s) exist in Stripe ` +
          `with this email. Use "Relink from Stripe" if one of them has the real subscription.`,
      });
    }
    return res.json({ configured: true, db, customer: null, candidates, checks });
  }

  let customer;
  try {
    customer = await stripe.customers.retrieve(account.stripe_customer_id, {
      expand: ['invoice_settings.default_payment_method'],
    });
  } catch (err) {
    if (err?.code === 'resource_missing') {
      checks.push({
        level: 'error',
        message: `Customer ${account.stripe_customer_id} doesn't exist in this Stripe account (wrong mode/key, or deleted).`,
      });
      return res.json({ configured: true, db, customer: null, checks });
    }
    throw err;
  }

  if (customer.deleted) {
    checks.push({ level: 'error', message: 'This Stripe customer has been deleted in Stripe.' });
    return res.json({ configured: true, db, customer: { id: customer.id, deleted: true }, checks });
  }

  const livemode = customer.livemode;

  const [subsResult, invoicesResult, upcomingResult, cardsResult] = await Promise.allSettled([
    stripe.subscriptions.list({
      customer: customer.id,
      status: 'all',
      limit: 10,
      expand: ['data.default_payment_method'],
    }),
    stripe.invoices.list({ customer: customer.id, limit: 10 }),
    account.stripe_subscription_id
      ? stripe.invoices.retrieveUpcoming({
          customer: customer.id,
          subscription: account.stripe_subscription_id,
        })
      : Promise.resolve(null),
    stripe.paymentMethods.list({ customer: customer.id, type: 'card', limit: 3 }),
  ]);

  const errors = [];
  const subs = subsResult.status === 'fulfilled' ? subsResult.value.data : [];
  if (subsResult.status === 'rejected') errors.push(`subscriptions: ${subsResult.reason?.message}`);
  const invoices = invoicesResult.status === 'fulfilled' ? invoicesResult.value.data : [];
  if (invoicesResult.status === 'rejected') errors.push(`invoices: ${invoicesResult.reason?.message}`);
  // retrieveUpcoming throws for a canceled subscription — that's not
  // worth surfacing as an error, there's simply nothing upcoming.
  const upcoming = upcomingResult.status === 'fulfilled' ? upcomingResult.value : null;
  const cards = cardsResult.status === 'fulfilled' ? cardsResult.value.data : [];

  const linkedSub = subs.find((s) => s.id === account.stripe_subscription_id) || null;
  const liveSubs = subs.filter((s) => LIVE_SUBSCRIPTION_STATUSES.has(s.status));

  // Payment method: the subscription's own, then the customer default,
  // then any card on file.
  const paymentMethod =
    summarizePaymentMethod(linkedSub?.default_payment_method) ||
    summarizePaymentMethod(customer.invoice_settings?.default_payment_method) ||
    summarizePaymentMethod(cards[0]);

  // --- consistency checks ---
  if (account.stripe_subscription_id && !linkedSub && subsResult.status === 'fulfilled') {
    checks.push({
      level: 'error',
      message:
        `Our database points at subscription ${account.stripe_subscription_id}, which isn't on this customer in Stripe. ` +
        `"Relink from Stripe" will fix the link (or clear it if there's no live subscription).`,
    });
  }
  if (linkedSub && !LIVE_SUBSCRIPTION_STATUSES.has(linkedSub.status)) {
    checks.push({
      level: 'warn',
      message:
        `The linked subscription is "${linkedSub.status}" in Stripe but our database still treats the account as subscribed ` +
        `(the customer.subscription.deleted webhook probably never landed). "Relink from Stripe" will clear it.`,
    });
  }
  if (!account.stripe_subscription_id && liveSubs.length) {
    checks.push({
      level: 'error',
      message:
        `Stripe has a live subscription (${liveSubs[0].id}, ${liveSubs[0].status}) but our database has none linked — ` +
        `the checkout.session.completed webhook probably never landed. The account is being billed but treated as free. ` +
        `"Relink from Stripe" will fix it.`,
    });
  }
  if (liveSubs.length > 1) {
    checks.push({
      level: 'warn',
      message: `This customer has ${liveSubs.length} live subscriptions in Stripe — they may be double-billed.`,
    });
  }
  if (linkedSub) {
    const storageItem = linkedSub.items.data.find((i) => i.id === account.stripe_subscription_item_id);
    if (!storageItem) {
      checks.push({
        level: 'warn',
        message:
          `The stored subscription item id doesn't match any item on the subscription, so pledge changes ` +
          `won't update billing. "Relink from Stripe" will fix it.`,
      });
    } else {
      const expectedQuantity = Math.max(1, await pledgedGBForAccount(account.id));
      if (storageItem.quantity !== expectedQuantity) {
        checks.push({
          level: 'warn',
          message:
            `Stripe is billing for ${storageItem.quantity} GB of storage, but this account currently has ` +
            `${expectedQuantity} GB pledged. "Resync quantity" will correct it.`,
        });
      }
    }
    if ((linkedSub.status === 'past_due' || linkedSub.status === 'unpaid') && account.relay_status === 'active') {
      checks.push({
        level: 'info',
        message:
          `Subscription is ${linkedSub.status} — Stripe is still retrying. Relay stays active until retries are ` +
          `exhausted, then pauses automatically.`,
      });
    }
  }
  if (account.relay_status !== 'active' && invoices[0]?.status === 'paid') {
    checks.push({
      level: 'info',
      message:
        'Relay is paused but the most recent invoice is paid — if the pause was for a failed payment, ' +
        'it may be safe to resume relay.',
    });
  }
  if (liveSubs.length && !paymentMethod) {
    checks.push({ level: 'warn', message: 'No payment method on file for a live subscription.' });
  }

  res.json({
    configured: true,
    livemode,
    db,
    customer: {
      id: customer.id,
      email: customer.email,
      name: customer.name,
      created: customer.created,
      balance: customer.balance,
      currency: customer.currency,
      delinquent: customer.delinquent,
      dashboard_url: stripeDashboardUrl(livemode, `customers/${customer.id}`),
    },
    payment_method: paymentMethod,
    subscriptions: subs.map((s) => ({
      ...summarizeSubscription(s, livemode),
      is_linked: s.id === account.stripe_subscription_id,
    })),
    upcoming_invoice: upcoming
      ? {
          amount_due: upcoming.amount_due,
          currency: upcoming.currency,
          next_payment_attempt: upcoming.next_payment_attempt,
          period_end: upcoming.period_end,
        }
      : null,
    invoices: invoices.map((inv) => ({
      id: inv.id,
      number: inv.number,
      status: inv.status,
      amount_due: inv.amount_due,
      amount_paid: inv.amount_paid,
      currency: inv.currency,
      created: inv.created,
      attempt_count: inv.attempt_count,
      next_payment_attempt: inv.next_payment_attempt,
      hosted_invoice_url: inv.hosted_invoice_url,
      dashboard_url: stripeDashboardUrl(livemode, `invoices/${inv.id}`),
    })),
    checks,
    errors,
  });
});

// Re-derives the account's Stripe link from what Stripe actually has —
// the fix for a missed checkout.session.completed or
// customer.subscription.deleted webhook. Finds the customer (stored id, or
// by email if none stored — refusing to guess between several), picks its
// live subscription, and writes customer/subscription/item ids back. If
// there's no live subscription, the subscription link is cleared.
adminRouter.post('/admin/accounts/:id/stripe/relink', async (req, res) => {
  if (!stripeConfigured()) return res.status(503).json({ error: 'Stripe is not configured' });
  const account = await loadAccountOr404(req, res, 'id, email, stripe_customer_id');
  if (!account) return;

  let customerId = account.stripe_customer_id;
  if (!customerId) {
    const found = await stripe.customers.list({ email: account.email, limit: 5 });
    // Prefer the one our checkout created (metadata.account_id).
    const ours = found.data.find((c) => c.metadata?.account_id === account.id);
    if (ours) customerId = ours.id;
    else if (found.data.length === 1) customerId = found.data[0].id;
    else if (found.data.length === 0) {
      return res.status(404).json({ error: 'No Stripe customer found with this email.' });
    } else {
      return res.status(409).json({
        error: `${found.data.length} Stripe customers share this email — link the right one by hand in the database.`,
      });
    }
  }

  const subs = await stripe.subscriptions.list({ customer: customerId, status: 'all', limit: 10 });
  const live = subs.data
    .filter((s) => LIVE_SUBSCRIPTION_STATUSES.has(s.status))
    .sort((a, b) => b.created - a.created);
  const sub = live[0] || null;
  const storageItem = sub
    ? sub.items.data.find((item) => item.price?.id === process.env.STRIPE_STORAGE_PRICE_ID) ??
      sub.items.data[0]
    : null;

  // Both ids are UNIQUE on accounts — if either already belongs to a
  // different account (same person signed up twice, say), say which one
  // rather than failing on the constraint.
  const conflict = await pool.query(
    `SELECT email FROM accounts
      WHERE id <> $1 AND (stripe_customer_id = $2 OR ($3::text IS NOT NULL AND stripe_subscription_id = $3))
      LIMIT 1`,
    [account.id, customerId, sub?.id ?? null],
  );
  if (conflict.rows.length) {
    return res.status(409).json({
      error: `That Stripe customer/subscription is already linked to ${conflict.rows[0].email} — sort out which account it belongs to first.`,
    });
  }

  await pool.query(
    `UPDATE accounts
        SET stripe_customer_id = $1,
            stripe_subscription_id = $2,
            stripe_subscription_item_id = $3
      WHERE id = $4`,
    [customerId, sub?.id ?? null, storageItem?.id ?? null, account.id],
  );

  logAdminAction(req, 'stripe-relink', `customer=${customerId} subscription=${sub?.id ?? 'none'}`);
  res.json({
    ok: true,
    customer_id: customerId,
    subscription_id: sub?.id ?? null,
    subscription_item_id: storageItem?.id ?? null,
    other_live_subscriptions: live.slice(1).map((s) => s.id),
  });
});

// Like syncSubscriptionQuantity() in billing.js, but reports failures
// instead of swallowing them, so the admin sees why it didn't work.
adminRouter.post('/admin/accounts/:id/stripe/resync-quantity', async (req, res) => {
  if (!stripeConfigured()) return res.status(503).json({ error: 'Stripe is not configured' });
  const account = await loadAccountOr404(req, res, 'id, stripe_subscription_item_id');
  if (!account) return;
  if (!account.stripe_subscription_item_id) {
    return res.status(400).json({ error: 'No subscription item is linked — try "Relink from Stripe" first.' });
  }

  const quantity = Math.max(1, await pledgedGBForAccount(account.id));
  await stripe.subscriptionItems.update(account.stripe_subscription_item_id, {
    quantity,
    proration_behavior: 'create_prorations',
  });
  logAdminAction(req, 'stripe-resync-quantity', `quantity=${quantity}`);
  res.json({ ok: true, quantity });
});

// mode: 'period_end' (cancel when the paid period ends), 'undo' (reverse a
// pending period-end cancel), or 'now' (cancel immediately — Stripe sends
// customer.subscription.deleted, which also clears the DB link; it's
// cleared here too so the UI is right without waiting on the webhook).
adminRouter.post('/admin/accounts/:id/stripe/cancel', async (req, res) => {
  if (!stripeConfigured()) return res.status(503).json({ error: 'Stripe is not configured' });
  const { mode } = req.body ?? {};
  if (!['period_end', 'undo', 'now'].includes(mode)) {
    return res.status(400).json({ error: "mode must be 'period_end', 'undo', or 'now'" });
  }
  const account = await loadAccountOr404(req, res, 'id, stripe_subscription_id');
  if (!account) return;
  if (!account.stripe_subscription_id) {
    return res.status(400).json({ error: 'No subscription is linked to this account.' });
  }

  if (mode === 'now') {
    await stripe.subscriptions.cancel(account.stripe_subscription_id);
    await pool.query(
      `UPDATE accounts
          SET stripe_subscription_id = NULL,
              stripe_subscription_item_id = NULL
        WHERE id = $1`,
      [account.id],
    );
  } else {
    await stripe.subscriptions.update(account.stripe_subscription_id, {
      cancel_at_period_end: mode === 'period_end',
    });
  }
  logAdminAction(req, `stripe-cancel-${mode}`, `subscription=${account.stripe_subscription_id}`);
  res.json({ ok: true });
});

// Remove one device, scoped to the account it's under so a copy-pasted
// device id can't accidentally reach across accounts.
adminRouter.delete('/admin/accounts/:id/devices/:deviceId', async (req, res) => {
  const result = await pool.query(
    `DELETE FROM devices WHERE id = $1 AND account_id = $2 RETURNING id, label`,
    [req.params.deviceId, req.params.id],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'device not found' });
  }
  logAdminAction(req, 'delete-device', `device=${req.params.deviceId} label=${JSON.stringify(result.rows[0].label)}`);
  res.status(204).end();
});

// Remove a buddy pairing — scoped to pairings this account is actually
// part of (as either side), same reasoning as the device route above.
//
// Ending a pairing changes what both sides are actually committed to, so
// (same as the normal invite-redeem flow in invites.js) both accounts'
// Stripe subscription quantities need resyncing afterward — missing this
// previously meant a paid account kept being billed for storage pledged to
// a pairing that no longer existed, with nothing to ever correct it short
// of an unrelated pledge change.
adminRouter.delete('/admin/accounts/:id/pairings/:pairingId', async (req, res) => {
  const result = await pool.query(
    `DELETE FROM buddy_pairings
      WHERE id = $1 AND (account_a_id = $2 OR account_b_id = $2)
      RETURNING id, account_a_id, account_b_id, pledged_bytes, status`,
    [req.params.pairingId, req.params.id],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'pairing not found' });
  }

  const { account_a_id, account_b_id, pledged_bytes, status } = result.rows[0];
  logAdminAction(
    req,
    'delete-pairing',
    `pairing=${req.params.pairingId} between=${account_a_id},${account_b_id} ` +
      `pledged_bytes=${pledged_bytes} status=${status}`,
  );
  await Promise.all([
    syncSubscriptionQuantity(account_a_id),
    syncSubscriptionQuantity(account_b_id),
  ]);

  res.status(204).end();
});

// Remove an invite the account created (not one it redeemed from someone
// else — that's the inviter's to remove).
adminRouter.delete('/admin/accounts/:id/invites/:inviteId', async (req, res) => {
  const result = await pool.query(
    `DELETE FROM invites WHERE id = $1 AND inviter_account_id = $2 RETURNING id, code, status`,
    [req.params.inviteId, req.params.id],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'invite not found' });
  }
  const invite = result.rows[0];
  logAdminAction(req, 'delete-invite', `invite=${invite.id} code=${invite.code} status=${invite.status}`);
  res.status(204).end();
});

// Delete the whole account. devices/buddy_pairings/invites(inviter
// side)/sessions/relay_usage_events all reference accounts with ON DELETE
// CASCADE (invites' redeemed_by_account_id instead SETs NULL — see
// db/migrations/009 — since that invite belongs to the inviter, not this
// account), so this one statement cleans up everything in Postgres. It
// does NOT touch Stripe — a subscription there keeps running and billing
// until it's cancelled separately in the Stripe dashboard (or via its
// API). had_stripe_subscription in the response flags when that manual
// step is needed; the dashboard UI warns on it too.
adminRouter.delete('/admin/accounts/:id', async (req, res) => {
  if (req.params.id === req.account.id) {
    return res.status(400).json({ error: "can't delete your own account from here" });
  }

  // Who this account is actively paired with, captured *before* the
  // delete cascades the pairing rows away — needed both to tell the admin
  // UI who's affected and to resync the survivor's Stripe quantity below
  // (ending a pairing from either side changes what both are actually
  // committed to; see the pairing-delete route above for the same fix).
  const activePairings = await pool.query(
    `SELECT other.email,
            CEIL(buddy_pairings.pledged_bytes / 1073741824.0) AS pledged_gb,
            CASE WHEN account_a_id = $1 THEN account_b_id ELSE account_a_id END AS other_account_id
       FROM buddy_pairings
       JOIN accounts other
         ON other.id = CASE WHEN account_a_id = $1 THEN account_b_id ELSE account_a_id END
      WHERE (account_a_id = $1 OR account_b_id = $1) AND status = 'active'`,
    [req.params.id],
  );

  const result = await pool.query(
    `DELETE FROM accounts WHERE id = $1 RETURNING id, email, stripe_subscription_id`,
    [req.params.id],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'account not found' });
  }
  // The row is gone after this, so the log line is the only record of whose
  // account it was.
  logAdminAction(
    req,
    'delete-account',
    `email=${result.rows[0].email} subscription=${result.rows[0].stripe_subscription_id ?? 'none'} ` +
      `ended_pairings=${activePairings.rows.length}`,
  );

  await Promise.all(
    activePairings.rows.map((row) => syncSubscriptionQuantity(row.other_account_id)),
  );

  res.json({
    ok: true,
    had_stripe_subscription: Boolean(result.rows[0].stripe_subscription_id),
    ended_active_pairings: activePairings.rows.map((row) => ({
      email: row.email,
      pledged_gb: Number(row.pledged_gb),
    })),
  });
});

// Admin-only error handler: surfaces the real message from a failed Stripe
// or SMTP call (e.g. "No such subscription", "Invalid login") instead of
// the global handler's generic "internal server error". Safe to expose
// here — every route on this router is already restricted to admins — and
// it's what makes these buttons usable for troubleshooting at all.
// eslint-disable-next-line no-unused-vars
adminRouter.use((err, req, res, next) => {
  console.error(`[admin] ${req.method} ${req.path} failed:`, err);
  const source = err?.type?.startsWith?.('Stripe') ? 'Stripe' : null;
  res.status(err?.statusCode && err.statusCode < 500 ? 400 : 502).json({
    error: `${source ? `${source}: ` : ''}${err?.message || 'request failed'}`,
  });
});
