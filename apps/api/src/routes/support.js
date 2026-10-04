import { Router } from 'express';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';
import { sendMail, SUPPORT_EMAIL } from '../lib/mailer.js';

// In-app "Contact support" form (dashboard.html). Logged-in only, so every
// request is already tied to a real account: the email to support@ carries
// the account's details automatically, and Reply-To is the account's own
// address, so replying from the support mailbox reaches the user directly.
// Nothing is stored — the email in support@ is the record. Locked-out users
// can't use this; they email support@ directly (linked from the login and
// password-reset pages).
export const supportRouter = Router();

export const SUPPORT_CATEGORIES = {
  account: 'Account or login',
  billing: 'Billing or payments',
  pairing: 'Buddy pairing',
  backup: 'Backups or restore',
  other: 'Something else',
};
const MIN_MESSAGE_LENGTH = 10;
const MAX_MESSAGE_LENGTH = 5000;
const MAX_SUBJECT_LENGTH = 120;

// Per-account limit, in memory: one API process, and a restart resetting it
// is harmless. Generous enough for "oh, one more thing", tight enough that
// the form can't be used to flood the support inbox.
const MAX_REQUESTS_PER_HOUR = 5;
const recentByAccount = new Map();
function overLimit(accountId) {
  const cutoff = Date.now() - 60 * 60 * 1000;
  const recent = (recentByAccount.get(accountId) || []).filter((t) => t > cutoff);
  recentByAccount.set(accountId, recent);
  return recent.length >= MAX_REQUESTS_PER_HOUR;
}

// Header-safe one-liner: no CR/LF (nodemailer guards against header
// injection too, but there's no reason to hand it newlines in a subject).
function oneLine(s, max) {
  return s.replace(/[\r\n\t]+/g, ' ').trim().slice(0, max);
}

supportRouter.post('/support', requireAuth, async (req, res) => {
  const { category, subject, message } = req.body ?? {};

  if (!Object.hasOwn(SUPPORT_CATEGORIES, category)) {
    return res.status(400).json({ error: 'Pick what this is about.' });
  }
  if (typeof message !== 'string' || message.trim().length < MIN_MESSAGE_LENGTH) {
    return res
      .status(400)
      .json({ error: `Please describe the problem (at least ${MIN_MESSAGE_LENGTH} characters).` });
  }
  if (message.length > MAX_MESSAGE_LENGTH) {
    return res
      .status(400)
      .json({ error: `That message is too long (max ${MAX_MESSAGE_LENGTH} characters).` });
  }
  if (subject !== undefined && typeof subject !== 'string') {
    return res.status(400).json({ error: 'Subject must be text.' });
  }

  const account = req.account;
  if (overLimit(account.id)) {
    return res.status(429).json({
      error: `You've sent several requests in the last hour. We'll get back to you; if it's urgent, email ${SUPPORT_EMAIL}.`,
    });
  }

  const [devices, pairings] = await Promise.all([
    pool.query(
      `SELECT label, created_at, last_seen_at
         FROM devices
        WHERE account_id = $1
        ORDER BY created_at`,
      [account.id],
    ),
    pool.query(
      `SELECT pledged_bytes
         FROM buddy_pairings
        WHERE (account_a_id = $1 OR account_b_id = $1)
          AND status = 'active'`,
      [account.id],
    ),
  ]);

  const pledgedGb = pairings.rows.reduce((sum, p) => sum + Number(p.pledged_bytes), 0) / 1024 ** 3;
  const signIn = [account.password_hash && 'password', account.google_sub && 'Google']
    .filter(Boolean)
    .join(' + ') || 'none';
  const deviceLines = devices.rows.length
    ? devices.rows
        .map(
          (d) =>
            `  - ${d.label || '(no label)'}: last seen ${
              d.last_seen_at ? new Date(d.last_seen_at).toISOString() : 'never'
            }`,
        )
        .join('\n')
    : '  (none)';

  const categoryLabel = SUPPORT_CATEGORIES[category];
  const subjectLine = oneLine(subject || '', MAX_SUBJECT_LENGTH);
  const emailSubject = `[Support] ${categoryLabel}${subjectLine ? `: ${subjectLine}` : ''} (${account.email})`;

  // Plain text only: the message is user-written, so an HTML part would mean
  // escaping it carefully for no real benefit in a support inbox.
  const text =
    `${message.trim()}\n\n` +
    `----------------------------------------\n` +
    `From:          ${account.email}${account.nickname ? ` (${account.nickname})` : ''}\n` +
    `Category:      ${categoryLabel}\n` +
    `Account ID:    ${account.id}\n` +
    `Member since:  ${new Date(account.created_at).toISOString().slice(0, 10)}\n` +
    `Sign-in:       ${signIn}\n` +
    `Email verified:${account.email_verified ? ' yes' : ' no'}\n` +
    `Plan:          ${account.stripe_subscription_id ? 'subscribed' : 'free'}\n` +
    `Relay status:  ${account.relay_status}${
      account.relay_paused_at
        ? ` (since ${new Date(account.relay_paused_at).toISOString().slice(0, 10)}, ${account.relay_paused_reason || 'no reason recorded'})`
        : ''
    }\n` +
    `Stripe customer: ${account.stripe_customer_id || '(none)'}\n` +
    `Active pairings: ${pairings.rows.length} (${pledgedGb.toFixed(0)} GB pledged)\n` +
    `Devices:\n${deviceLines}\n` +
    `Browser:       ${oneLine(String(req.get('user-agent') || 'unknown'), 200)}\n` +
    `----------------------------------------\n` +
    `Reply to this email to answer ${account.email} directly. ` +
    `Look the account up by email on /admin.html.`;

  let sent;
  try {
    sent = await sendMail({ to: SUPPORT_EMAIL, subject: emailSubject, text, replyTo: account.email });
  } catch (err) {
    console.error(`Support request from ${account.email} failed to send:`, err);
    return res.status(502).json({
      error: `We couldn't send that just now. Please try again, or email ${SUPPORT_EMAIL} directly.`,
    });
  }
  if (!sent) {
    return res.status(503).json({
      error: `The support form isn't available right now. Please email ${SUPPORT_EMAIL} directly.`,
    });
  }

  recentByAccount.get(account.id).push(Date.now());
  console.log(`[support] request from account=${account.id} category=${category}`);
  res.json({
    ok: true,
    message: `Thanks — your message is with us. We'll reply to ${account.email}.`,
  });
});
