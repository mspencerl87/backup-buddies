// Admin access is granted by email, not a database role/flag — the set of
// admins here is tiny and changes rarely enough that a deploy-time env var
// is simpler than building out real role management. ADMIN_EMAILS is a
// comma-separated list in .env; comparison is case-insensitive.
const adminEmails = new Set(
  (process.env.ADMIN_EMAILS || '')
    .split(',')
    .map((e) => e.trim().toLowerCase())
    .filter(Boolean),
);

export function isAdminEmail(email) {
  return typeof email === 'string' && adminEmails.has(email.toLowerCase());
}
