// Chain after requireAuth, which sets req.account. Gates pairing only —
// creating or redeeming an invite — not login or browsing the dashboard;
// see db/migrations/007_email_verification.sql for why. Keeping this as
// its own middleware (rather than folding into requireAuth) means every
// other authed route is untouched; only the routes that opt in via this
// are affected.
export function requireEmailVerified(req, res, next) {
  if (!req.account?.email_verified) {
    return res.status(403).json({
      error:
        'Verify your email before pairing with a buddy — check your inbox for the link, or resend it from your dashboard.',
      email_unverified: true,
    });
  }
  next();
}
