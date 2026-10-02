import { pool } from '../db/pool.js';
import { sendMail } from '../lib/mailer.js';
import { getAdminEmails } from '../lib/admin.js';
import { INACTIVITY_DAYS, isExempt } from './stale-account-cleanup.js';

// How long after the inactivity warning (jobs/stale-account-cleanup.js)
// an account has to show some activity before it's included in the next
// digest, and how often that digest goes out. Nothing here deletes an
// account or cancels a subscription — this only emails the admins a list
// so a person decides, from /admin.html's existing delete action (which
// already warns that it doesn't touch Stripe either — see routes/admin.js
// — same reasoning applies: reviewing this digest is the moment to cancel
// billing by hand too, before or alongside deleting).
const GRACE_PERIOD_DAYS = 30;
const REPORT_INTERVAL_DAYS = 30;
const REPORT_JOB_NAME = 'stale_account_report';

// Accounts that were warned, and are still showing no activity a full
// grace period later.
async function findAccountsPastGracePeriod() {
  const result = await pool.query(
    `SELECT accounts.id, accounts.email,
            (accounts.stripe_subscription_id IS NOT NULL) AS has_subscription,
            MAX(devices.last_seen_at) AS last_device_seen_at
       FROM accounts
       LEFT JOIN devices ON devices.account_id = accounts.id
      WHERE accounts.stale_warning_sent_at IS NOT NULL
        AND accounts.stale_warning_sent_at < now() - interval '${GRACE_PERIOD_DAYS} days'
      GROUP BY accounts.id
      ORDER BY accounts.stale_warning_sent_at`,
  );
  // Re-checked here too (not just at warning time) in case
  // STALE_CLEANUP_EXEMPT_EMAILS changed since the warning went out.
  return result.rows.filter((row) => !isExempt(row.email));
}

// Persisted in the DB (job_runs, migration 008) rather than kept in
// memory/via a bare setInterval — a restart or redeploy shouldn't cause
// this monthly email to fire early (or go silent for a cycle).
async function reportIsDue() {
  const result = await pool.query(`SELECT last_run_at FROM job_runs WHERE job_name = $1`, [
    REPORT_JOB_NAME,
  ]);
  const lastRunAt = result.rows[0]?.last_run_at;
  if (!lastRunAt) return true;
  const daysSince = (Date.now() - new Date(lastRunAt).getTime()) / (1000 * 60 * 60 * 24);
  return daysSince >= REPORT_INTERVAL_DAYS;
}

async function markReportRun() {
  await pool.query(
    `INSERT INTO job_runs (job_name, last_run_at) VALUES ($1, now())
     ON CONFLICT (job_name) DO UPDATE SET last_run_at = now()`,
    [REPORT_JOB_NAME],
  );
}

async function sendAdminDigest(accounts) {
  const admins = getAdminEmails();
  if (admins.length === 0) {
    console.warn('Stale-account digest: ADMIN_EMAILS is empty — nothing to send to.');
    return;
  }

  const appUrl = process.env.APP_URL || '';
  const lines = accounts.map((a) => {
    const lastSeen = a.last_device_seen_at
      ? new Date(a.last_device_seen_at).toISOString().slice(0, 10)
      : 'never';
    const sub = a.has_subscription ? 'has an active subscription' : 'no subscription';
    return `- ${a.email} (id ${a.id}) — last device activity: ${lastSeen}, ${sub}`;
  });

  const totalDays = INACTIVITY_DAYS + GRACE_PERIOD_DAYS;
  const text =
    `${accounts.length} account(s) have shown no device activity for over ${totalDays} days total ` +
    `(warned after ${INACTIVITY_DAYS} days, then a ${GRACE_PERIOD_DAYS}-day grace period with still ` +
    `nothing). Nothing has been changed automatically — review each one and delete it from ` +
    `${appUrl}/admin.html if appropriate. Remember deleting an account there does NOT cancel its ` +
    `Stripe subscription — cancel that separately in the Stripe dashboard first if it has one.\n\n` +
    lines.join('\n');

  for (const to of admins) {
    try {
      await sendMail({
        to,
        subject: `Backup Buddies: ${accounts.length} stale account(s) to review`,
        text,
      });
    } catch (err) {
      console.error(`Failed to send stale-account digest to admin ${to}:`, err);
    }
  }
}

// Checks whether a month has passed since the last digest and, if so,
// emails the admins the current list of accounts past their grace period.
// Safe to call more often than monthly (e.g. daily, alongside
// runStaleAccountCleanup) — it's a no-op whenever reportIsDue() is false.
export async function sendStaleAccountDigest() {
  if (!(await reportIsDue())) return { sent: false, candidateCount: 0 };

  const accounts = await findAccountsPastGracePeriod();
  if (accounts.length > 0) {
    await sendAdminDigest(accounts);
  }
  await markReportRun();
  return { sent: accounts.length > 0, candidateCount: accounts.length };
}
