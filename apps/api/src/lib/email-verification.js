import crypto from 'node:crypto';
import { pool } from '../db/pool.js';
import { sendMail } from './mailer.js';

// How long a verification link stays valid, and the minimum gap between
// two sends to the same account — the latter stops a "resend" button from
// being hammered into flooding someone's inbox (or tripping Purelymail's
// own sending limits).
const TOKEN_TTL_HOURS = 48;
const RESEND_COOLDOWN_SECONDS = 60;

// Generates a fresh token, stores only its hash (same pattern as device
// tokens — see migration 006 / routes/devices.js's token_hash), and emails
// the raw token as a link. Returns one of:
//   'sent'             — a new link was generated and emailed
//   'already_verified' — nothing to do, account is already verified
//   'cooldown'         — asked to resend too soon; the previous link is
//                        still valid, so there's no need for a new one yet
export async function sendVerificationEmail(account) {
  if (account.email_verified) return 'already_verified';

  if (account.email_verification_sent_at) {
    const secondsSinceLast =
      (Date.now() - new Date(account.email_verification_sent_at).getTime()) / 1000;
    if (secondsSinceLast < RESEND_COOLDOWN_SECONDS) return 'cooldown';
  }

  const token = crypto.randomBytes(32).toString('hex');
  const tokenHash = crypto.createHash('sha256').update(token).digest('hex');

  await pool.query(
    `UPDATE accounts
        SET email_verification_token_hash = $1,
            email_verification_sent_at = now()
      WHERE id = $2`,
    [tokenHash, account.id],
  );

  const verifyUrl = `${process.env.APP_URL}/verify-email.html?token=${token}`;
  await sendMail({
    to: account.email,
    subject: 'Verify your Backup Buddies email',
    text:
      `Confirm your email to pair with a buddy:\n\n${verifyUrl}\n\n` +
      `This link expires in ${TOKEN_TTL_HOURS} hours. If you didn't sign up for Backup Buddies, you can ignore this.`,
    html:
      `<p>Confirm your email to pair with a buddy:</p>` +
      `<p><a href="${verifyUrl}">${verifyUrl}</a></p>` +
      `<p>This link expires in ${TOKEN_TTL_HOURS} hours. If you didn't sign up for Backup Buddies, you can ignore this.</p>`,
  });

  return 'sent';
}

// Looks the token up by its hash, checks it hasn't expired, and marks the
// account verified. Tokens are single-use — the hash is cleared on success,
// so the same emailed link can't be replayed. Returns one of:
//   'verified', 'invalid' (no match / malformed), 'expired'
export async function verifyEmailToken(token) {
  if (typeof token !== 'string' || token.length === 0) return 'invalid';

  const tokenHash = crypto.createHash('sha256').update(token).digest('hex');
  const result = await pool.query(
    `SELECT id, email_verification_sent_at
       FROM accounts
      WHERE email_verification_token_hash = $1`,
    [tokenHash],
  );
  const account = result.rows[0];
  if (!account) return 'invalid';

  const ageHours =
    (Date.now() - new Date(account.email_verification_sent_at).getTime()) / (1000 * 60 * 60);
  if (ageHours > TOKEN_TTL_HOURS) return 'expired';

  await pool.query(
    `UPDATE accounts
        SET email_verified = true,
            email_verification_token_hash = NULL
      WHERE id = $1`,
    [account.id],
  );
  return 'verified';
}
