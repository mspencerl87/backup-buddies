import crypto from 'node:crypto';
import bcrypt from 'bcryptjs';
import { pool } from '../db/pool.js';
import { sendMail } from './mailer.js';

// Password reset links. Today these are only issued by an admin from the
// account-detail view (routes/admin.js) — there's no self-service "forgot
// password" form yet, but this module is written so one can call
// createPasswordResetLink() directly when it's added.
//
// Mirrors lib/email-verification.js: a random token is emailed, only its
// SHA-256 is stored (migration 013), and it's single-use.
const TOKEN_TTL_HOURS = 24;
const MIN_PASSWORD_LENGTH = 8; // same rule as /signup in routes/auth.js

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
  // A Google-only account has no password yet — the same link lets it set
  // one, so the wording covers both cases rather than saying "reset".
  const emailed = await sendMail({
    to: account.email,
    subject: 'Reset your Backup Buddies password',
    text:
      `A password reset was requested for your Backup Buddies account. ` +
      `Choose a new password here:\n\n${url}\n\n` +
      `This link expires in ${TOKEN_TTL_HOURS} hours and can only be used once. ` +
      `If you didn't ask for this, you can ignore this email — your current password still works.`,
    html:
      `<p>A password reset was requested for your Backup Buddies account. Choose a new password here:</p>` +
      `<p><a href="${url}">${url}</a></p>` +
      `<p>This link expires in ${TOKEN_TTL_HOURS} hours and can only be used once. ` +
      `If you didn't ask for this, you can ignore this email — your current password still works.</p>`,
  });

  return { url, emailed, expires_in_hours: TOKEN_TTL_HOURS };
}

// Returns 'reset', 'invalid', 'expired', or 'weak_password'. On success the
// token is cleared and every existing session for the account is deleted,
// so anyone still signed in with the old password is logged out.
export async function resetPasswordWithToken(token, newPassword) {
  if (typeof token !== 'string' || token.length === 0) return 'invalid';
  if (typeof newPassword !== 'string' || newPassword.length < MIN_PASSWORD_LENGTH) {
    return 'weak_password';
  }

  const result = await pool.query(
    `SELECT id, password_reset_sent_at
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
              password_reset_sent_at = NULL
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

  return 'reset';
}
