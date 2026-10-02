import { isAdminEmail } from '../lib/admin.js';

// Chain after requireAuth, which sets req.account. Keeping this as a
// separate middleware (rather than folding the check into requireAuth)
// means every other route stays untouched — only routes that explicitly
// opt in via requireAdmin are affected by the ADMIN_EMAILS list at all.
export function requireAdmin(req, res, next) {
  if (!req.account || !isAdminEmail(req.account.email)) {
    return res.status(403).json({ error: 'admin access required' });
  }
  next();
}
