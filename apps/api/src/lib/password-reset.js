import crypto from 'node:crypto';
import bcrypt from 'bcryptjs';
import { pool } from '../db/pool.js';
import { sendMail } from './mailer.js';

// Password reset links, issued two ways:
//   - by an admin from the account-detail view (routes/admin.js →
//     createPasswordResetLink), and
//   - self-service from forgot-password.html (routes/auth.js →
//     requestPasswordReset, which calls createPasswordResetLink).
// Both are redeemed by reset-password.html → resetPasswordWithToken.
//
// Mirrors lib/email-verification.js: a random token is emailed, only its
// SHA-256 is stored (migration 013), and it's single-use. One TTL covers
// both kinds of link, since they share the same column.
const TOKEN_TTL_HOURS = 24;
const MIN_PASSWORD_LENGTH = 8; // same rule as /signup in routes/auth.js

// Self-service only: minimum gap between two "forgot password" emails to the
// same account, so the form can't be used to flood someone's inbox.
const SELF_SERVICE_COOLDOWN_SECONDS = 60;
// Self-service only: crude global cap on forgot-password emails across *all*
// accounts per rolling minute. The per-account cooldown doesn't stop someone
// spraying the form with many different addresses, which would mail a lot
// of strangers and could trip Purelymail's own sending limits. In-memory is
// fine: one API process, and a restart resetting the counter is harmless.
// (Per-IP limiting would be better, but the API sits behind Caddy and
// Cloudflare without `trust proxy` configured, so req.ip isn't the real
// client — see docs/TESTING-CHECKLIST.md.)
const SELF_SERVICE_SENDS_PER_MINUTE = 20;

export const SUPPORT_EMAIL = process.env.SUPPORT_EMAIL || 'support@filegarden.net';

const BACKUPS_NOTE =
  "Your backups aren't affected by this: files are encrypted with the passphrase on your own device, " +
  'which is separate from your account password.';

let recentSelfServiceSends = [];
function selfServiceCapReached() {
  const cutoff = Date.now() - 60_000;
  recentSelfServiceSends = recentSelfServiceSends.filter((t) => t > cutoff);
  return recentSelfServiceSends.length >= SELF_SERVICE_SENDS_PER_MINUTE;
}

function hashToken(token) {
  return crypto.createHash('sha256').update(token).digest('hex');
}

// Generates a fresh link (invalidating any earlier unused one for this
// account) and tries to email it. Returns { url, emailed, expires_in_hours }.
// `emailed` is false when SMTP isn't configured on this deployment — the
// admin UI then shows the link so it can be sent another way (e.g. pasted
// into a support ticket). Throws if SMTP is configured but sending fails,
// so the admin sees the real error rather than a false "sent".
export async function createPasswordResetLink(account) {
  const token = crypto.randomBytes(32).toString('hex');

  await pool.query(
    `UPDATE accounts
        SET password_reset_token_hash = $1,
            password_reset_sent_at = now()
      WHERE id = $2`,
    [hashToken(token), account.id],
  );

  const url = `${process.env.APP_URL}/reset-password.html?token=${token}`;
  // A Google-only account has no password yet — an admin-issued link lets it
  // set one, so the wording covers both cases rather than saying "reset".
  const emailed = await sendMail({
    to: account.email,
    subject: 'Reset your Backup Buddies password',
    text:
      `A password reset was requested for your Backup Buddies account. ` +
      `Choose a new password here:\n\n${url}\n\n` +
      `This link expires in ${TOKEN_TTL_HOURS} hours and can only be used once. ${BACKUPS_NOTE}\n\n` +
      `If you didn't ask for this, you can ignore this email — your current password still works. ` +
      `Questions? Email ${SUPPORT_EMAIL}.`,
    html:
      `<p>A password reset was requested for your Backup Buddies account. Choose a new password here:</p>` +
      `<p><a href="${url}">${url}</a></p>` +
      `<p>This link expires in ${TOKEN_TTL_HOURS} hours and can only be used once. ${BACKUPS_NOTE}</p>` +
      `<p>If you didn't ask for this, you can ignore this email — your current password still works. ` +
      `Questions? Email <a href="mailto:${SUPPORT_EMAIL}">${SUPPORT_EMAIL}</a>.</p>`,
  });

  return { url, emailed, expires_in_hours: TOKEN_TTL_HOURS };
}

// Self-service "forgot password" for an email address. Deliberately returns
// nothing: the route always gives the same response whether or not the
// address belongs to an account, so the form can't be used to find out
// who's registered. For the same reason, callers shouldn't await this — the
// SMTP round trip would otherwise make "account exists" measurably slower
// than "no such account".
export async function requestPasswordReset(email) {
  if (typeof email !== 'string') return;
  const normalizedEmail = email.trim().toLowerCase();
  if (!normalizedEmail) return;

  const result = await pool.query(
    `SELECT id, email, password_hash, password_reset_sent_at
       FROM accounts
      WHERE email = $1`,
    [normalizedEmail],
  );
  const account = result.rows[0];
  if (!account) return;

  if (account.password_reset_sent_at) {
    const secondsSinceLast =
      (Date.now() - new Date(account.password_reset_sent_at).getTime()) / 1000;
    if (secondsSinceLast < SELF_SERVICE_COOLDOWN_SECONDS) return;
  }
  if (selfServiceCapReached()) {
    console.warn(`Password reset global send cap reached; skipped email to ${account.email}`);
    return;
  }
  recentSelfServiceSends.push(Date.now());

  // Google-only account: point them at "Continue with Google" instead of
  // emailing a link to add a password. (An admin can still issue one from
  // the account-detail view if they genuinely want a password too.) Still
  // stamps sent_at, without a token, so the cooldown applies.
  if (!account.password_hash) {
    await pool.query(
      `UPDATE accounts SET password_reset_sent_at = now() WHERE id = $1`,
      [account.id],
    );
    const loginUrl = `${process.env.APP_URL}/login.html`;
    await sendMail({
      to: account.email,
      subject: 'Signing in to Backup Buddies',
      text:
        `Someone asked to reset the password for this Backup Buddies account. ` +
        `This account signs in with Google and doesn't have a password, so there's nothing to reset — ` +
        `use "Continue with Google" on the login page:\n\n${loginUrl}\n\n` +
        `If you didn't ask for this, you can ignore it. Questions? Email ${SUPPORT_EMAIL}.`,
      html:
        `<p>Someone asked to reset the password for this Backup Buddies account. ` +
        `This account signs in with Google and doesn't have a password, so there's nothing to reset — ` +
        `use <strong>Continue with Google</strong> on the login page:</p>` +
        `<p><a href="${loginUrl}">${loginUrl}</a></p>` +
        `<p>If you didn't ask for this, you can ignore it. Questions? Email ` +
        `<a href="mailto:${SUPPORT_EMAIL}">${SUPPORT_EMAIL}</a>.</p>`,
    });
    return;
  }

  await createPasswordResetLink(account);
}

// Returns 'reset', 'invalid', 'expired', or 'weak_password'. On success the
// token is cleared, every existing session for the account is deleted (so
// anyone still signed in with the old password is logged out), the email
// is marked verified (the link proves the person controls the inbox), and a
// "password was changed" notice is emailed. Device tokens aren't touched, so
// running clients keep syncing.
export async function resetPasswordWithToken(token, newPassword) {
  if (typeof token !== 'string' || token.length === 0) return 'invalid';
  if (typeof newPassword !== 'string' || newPassword.length < MIN_PASSWORD_LENGTH) {
    return 'weak_password';
  }

  const result = await pool.query(
    `SELECT id, email, password_reset_sent_at
       FROM accounts
      WHERE password_reset_token_hash = $1`,
    [hashToken(token)],
  );
  const account = result.rows[0];
  if (!account) return 'invalid';

  const ageHours =
    (Date.now() - new Date(account.password_reset_sent_at).getTime()) / (1000 * 60 * 60);
  if (ageHours > TOKEN_TTL_HOURS) return 'expired';

  const passwordHash = await bcrypt.hash(newPassword, 12);

  const client = await pool.connect();
  try {
    await client.query('BEGIN');
    // The token-hash match in the WHERE clause makes this race-safe: if two
    // requests redeem the same link at once, only one UPDATE matches.
    const updated = await client.query(
      `UPDATE accounts
          SET password_hash = $1,
              password_reset_token_hash = NULL,
              password_reset_sent_at = NULL,
              email_verified = true
        WHERE id = $2 AND password_reset_token_hash = $3
        RETURNING id`,
      [passwordHash, account.id, hashToken(token)],
    );
    if (updated.rows.length === 0) {
      await client.query('ROLLBACK');
      return 'invalid';
    }
    await client.query('DELETE FROM sessions WHERE account_id = $1', [account.id]);
    await client.query('COMMIT');
  } catch (err) {
    await client.query('ROLLBACK');
    throw err;
  } finally {
    client.release();
  }

  // Heads-up to the account holder, so a reset they didn't make doesn't go
  // unnoticed. Best-effort: the password has already changed either way.
  sendMail({
    to: account.email,
    subject: 'Your Backup Buddies password was changed',
    text:
      `The password for your Backup Buddies account was just changed, and you were signed out of the website everywhere. ` +
      `If this was you, there's nothing else to do.\n\n` +
      `If it wasn't, reset your password again right away and email ${SUPPORT_EMAIL}.`,
    html:
      `<p>The password for your Backup Buddies account was just changed, and you were signed out of the website everywhere. ` +
      `If this was you, there's nothing else to do.</p>` +
      `<p>If it wasn't, reset your password again right away and email ` +
      `<a href="mailto:${SUPPORT_EMAIL}">${SUPPORT_EMAIL}</a>.</p>`,
  }).catch((err) => {
    console.error(`Failed to send password-changed notice to ${account.email}:`, err);
  });

  return 'reset';
}
