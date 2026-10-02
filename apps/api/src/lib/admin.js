// Admin access is granted by email, not a database role/flag — the set of
// admins here is tiny and changes rarely enough that a deploy-time env var
// is simpler than building out real role management. ADMIN_EMAILS is a
// comma-separated list in .env; comparison is case-insensitive.
const ADMIN_EMAILS = (process.env.ADMIN_EMAILS || '')
  .split(',')
  .map((e) => e.trim().toLowerCase())
  .filter(Boolean);
const adminEmailSet = new Set(ADMIN_EMAILS);

export function isAdminEmail(email) {
  return typeof email === 'string' && adminEmailSet.has(email.toLowerCase());
}

// For jobs/stale-account-digest.js — who the monthly "accounts ready for
// review" email goes to.
export function getAdminEmails() {
  return ADMIN_EMAILS;
}
