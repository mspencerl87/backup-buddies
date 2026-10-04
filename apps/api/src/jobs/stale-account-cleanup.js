import { pool } from '../db/pool.js';
import { sendMail, SUPPORT_EMAIL } from '../lib/mailer.js';
import { isAdminEmail } from '../lib/admin.js';

// "1 month of no activity" before the account holder gets a warning email
// — see db/migrations/008_*.sql. What happens after that (a monthly digest
// to the admins, never an automatic delete) lives in
// jobs/stale-account-digest.js.
export const INACTIVITY_DAYS = 30;

// Accounts this job should never flag, regardless of how inactive they
// look — same comma-separated-emails pattern as ADMIN_EMAILS (lib/admin.js).
// Meant for accounts kept around on purpose for troubleshooting (e.g. the
// testa/testb pairing) that would otherwise look abandoned to this job.
// Admin accounts are always exempt too. Shared with stale-account-digest.js.
const exemptEmails = new Set(
  (process.env.STALE_CLEANUP_EXEMPT_EMAILS || '')
    .split(',')
    .map((e) => e.trim().toLowerCase())
    .filter(Boolean),
);
export function isExempt(email) {
  return isAdminEmail(email) || (typeof email === 'string' && exemptEmails.has(email.toLowerCase()));
}

// "Activity" = any device belonging to the account checking in
// (devices.last_seen_at, updated every ~30s by a running client via
// GET /devices/me/buddies — see routes/devices.js). That's "has a client
// actually been running," which fits what this service is for better than
// dashboard logins alone. An account with no devices yet is measured from
// its own created_at, so a brand-new signup isn't instantly flagged.
async function findNewlyStaleAccounts() {
  const result = await pool.query(
    `SELECT accounts.id, accounts.email
       FROM accounts
       LEFT JOIN devices ON devices.account_id = accounts.id
      WHERE accounts.stale_warning_sent_at IS NULL
      GROUP BY accounts.id
     HAVING GREATEST(accounts.created_at, COALESCE(MAX(devices.last_seen_at), accounts.created_at))
            < now() - interval '${INACTIVITY_DAYS} days'`,
  );
  return result.rows.filter((row) => !isExempt(row.email));
}

// An account that got a warning but has since shown activity (a client
// started running again) has its clock reset — it drops off the stale list
// entirely and goes back through the normal check from scratch if it goes
// quiet again later. This is also what keeps an account out of the monthly
// admin digest once its owner comes back.
async function clearResolvedWarnings() {
  const result = await pool.query(
    `SELECT accounts.id
       FROM accounts
       LEFT JOIN devices ON devices.account_id = accounts.id
      WHERE accounts.stale_warning_sent_at IS NOT NULL
      GROUP BY accounts.id, accounts.stale_warning_sent_at
     HAVING COALESCE(MAX(devices.last_seen_at), '-infinity'::timestamptz)
            > accounts.stale_warning_sent_at`,
  );
  if (result.rows.length === 0) return 0;

  await pool.query(`UPDATE accounts SET stale_warning_sent_at = NULL WHERE id = ANY($1::uuid[])`, [
    result.rows.map((row) => row.id),
  ]);
  return result.rows.length;
}

async function warnStaleAccount(account) {
  // Set the timestamp first — if the email send below fails, we'd still
  // rather the clock started now (and get picked up by tomorrow's
  // "resolved" check if they come back) than silently retry a fresh
  // warning every day forever because sendMail keeps throwing.
  await pool.query(`UPDATE accounts SET stale_warning_sent_at = now() WHERE id = $1`, [account.id]);

  try {
    await sendMail({
      to: account.email,
      subject: 'Your Backup Buddies account has been inactive',
      text:
        `We haven't seen a backup client running on your account in over ${INACTIVITY_DAYS} days.\n\n` +
        `Accounts that stay inactive are periodically reviewed by hand for removal — including any ` +
        `buddy pairings and the data stored with a buddy — so nothing happens to yours without someone ` +
        `actually looking first, but it's worth checking in.\n\n` +
        `To keep your account, just run your backup client again (or check ` +
        `${process.env.APP_URL || ''}/setup.html if it's stopped working) — no other action is needed.\n\n` +
        `If you meant to stop using Backup Buddies, you don't need to do anything further.` +
        `\n\nQuestions? Just reply to this email (it goes to ${SUPPORT_EMAIL}).`,
      replyTo: SUPPORT_EMAIL,
      html:
        `<p>We haven't seen a backup client running on your account in over ${INACTIVITY_DAYS} days.</p>` +
        `<p>Accounts that stay inactive are periodically reviewed by hand for removal — including any ` +
        `buddy pairings and the data stored with a buddy — so nothing happens to yours without someone ` +
        `actually looking first, but it's worth checking in.</p>` +
        `<p>To keep your account, just run your backup client again (or check ` +
        `<a href="${process.env.APP_URL || ''}/setup.html">your setup</a> if it's stopped working) — no ` +
        `other action is needed.</p>` +
        `<p>If you meant to stop using Backup Buddies, you don't need to do anything further.</p>` +
        `<p>Questions? Just reply to this email (it goes to <a href="mailto:${SUPPORT_EMAIL}">${SUPPORT_EMAIL}</a>).</p>`,
    });
  } catch (err) {
    console.error(`Failed to send stale-account warning to ${account.email}:`, err);
  }
}

// Daily: clear warnings for accounts that came back, and warn any newly
// -stale ones. Nothing destructive happens here — see
// jobs/stale-account-digest.js for what (doesn't) happen next.
export async function runStaleAccountCleanup() {
  const cleared = await clearResolvedWarnings();
  const toWarn = await findNewlyStaleAccounts();
  for (const account of toWarn) {
    await warnStaleAccount(account);
  }
  return { cleared, warned: toWarn.length };
}
