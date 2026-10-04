import { Router } from 'express';
import crypto from 'crypto';
import bcrypt from 'bcryptjs';
import { OAuth2Client } from 'google-auth-library';
import { pool } from '../db/pool.js';
import { SESSION_COOKIE, requireAuth } from '../middleware/require-auth.js';
import { sendVerificationEmail, verifyEmailToken } from '../lib/email-verification.js';
import { resetPasswordWithToken } from '../lib/password-reset.js';

export const authRouter = Router();

const SESSION_TTL_DAYS = 30;
const OAUTH_STATE_COOKIE = 'oauth_state';
const googleClient = process.env.GOOGLE_CLIENT_ID
  ? new OAuth2Client(process.env.GOOGLE_CLIENT_ID)
  : null;

function setSessionCookie(res, sessionId) {
  res.cookie(SESSION_COOKIE, sessionId, {
    httpOnly: true,
    secure: true, // only sent over HTTPS — fine, Caddy terminates TLS in front of this
    sameSite: 'lax',
    maxAge: SESSION_TTL_DAYS * 24 * 60 * 60 * 1000,
  });
}

async function createSession(accountId) {
  const result = await pool.query(
    `INSERT INTO sessions (account_id, expires_at)
     VALUES ($1, now() + interval '${SESSION_TTL_DAYS} days')
     RETURNING id`,
    [accountId],
  );
  return result.rows[0].id;
}

authRouter.post('/signup', async (req, res) => {
  const { email, password, accepted_terms } = req.body ?? {};

  if (typeof email !== 'string' || typeof password !== 'string') {
    return res.status(400).json({ error: 'email and password are required' });
  }
  if (password.length < 8) {
    return res.status(400).json({ error: 'password must be at least 8 characters' });
  }
  if (accepted_terms !== true) {
    return res.status(400).json({ error: 'you must accept the Terms of Service to sign up' });
  }

  const normalizedEmail = email.trim().toLowerCase();
  const passwordHash = await bcrypt.hash(password, 12);

  try {
    const result = await pool.query(
      `INSERT INTO accounts (email, password_hash, tier, terms_accepted_at)
       VALUES ($1, $2, 'free', now())
       RETURNING id, email, tier, created_at`,
      [normalizedEmail, passwordHash],
    );

    const account = result.rows[0];
    const sessionId = await createSession(account.id);
    setSessionCookie(res, sessionId);

    // Best-effort — a signup should never fail just because SMTP hiccuped.
    // The dashboard offers a "resend" button (see /auth/resend-verification
    // below) for whenever this didn't land.
    try {
      await sendVerificationEmail({
        id: account.id,
        email: account.email,
        email_verified: false,
        email_verification_sent_at: null,
      });
    } catch (err) {
      console.error(`Failed to send verification email to ${account.email}:`, err);
    }

    res.status(201).json({ account: { ...account, email_verified: false } });
  } catch (err) {
    if (err.code === '23505') {
      // unique_violation on accounts.email
      return res.status(409).json({ error: 'an account with that email already exists' });
    }
    throw err;
  }
});

authRouter.post('/login', async (req, res) => {
  const { email, password } = req.body ?? {};

  if (typeof email !== 'string' || typeof password !== 'string') {
    return res.status(400).json({ error: 'email and password are required' });
  }

  const normalizedEmail = email.trim().toLowerCase();
  const result = await pool.query(
    `SELECT id, email, tier, password_hash FROM accounts WHERE email = $1`,
    [normalizedEmail],
  );

  // Always run bcrypt.compare even on a missing account, against a fixed
  // dummy hash — otherwise a "no such account" response returns faster
  // than a "wrong password" one, letting an attacker enumerate which
  // emails are registered by timing alone.
  const account = result.rows[0];
  const hashToCheck = account?.password_hash ?? '$2a$12$invalidsaltinvalidsaltinvalidsaltinvalidsalt';
  const passwordMatches = await bcrypt.compare(password, hashToCheck);

  if (!account || !passwordMatches) {
    return res.status(401).json({ error: 'invalid email or password' });
  }

  const sessionId = await createSession(account.id);
  setSessionCookie(res, sessionId);

  res.json({
    account: { id: account.id, email: account.email, tier: account.tier },
  });
});

// --- Google sign-in ---
//
// Standard OAuth 2.0 authorization-code flow. We don't use Passport here —
// the whole thing is two endpoints and a token exchange, and keeping it
// inline avoids a dependency whose session/strategy model doesn't match
// the plain cookie-session approach the rest of this file already uses.

authRouter.get('/auth/google', (req, res) => {
  if (!googleClient || !process.env.GOOGLE_CLIENT_SECRET || !process.env.GOOGLE_REDIRECT_URI) {
    return res.redirect('/login.html?error=google_not_configured');
  }

  const state = crypto.randomBytes(16).toString('hex');
  res.cookie(OAUTH_STATE_COOKIE, state, {
    httpOnly: true,
    secure: true,
    sameSite: 'lax',
    maxAge: 10 * 60 * 1000, // 10 minutes — this cookie only needs to survive the redirect round trip
  });

  const params = new URLSearchParams({
    client_id: process.env.GOOGLE_CLIENT_ID,
    redirect_uri: process.env.GOOGLE_REDIRECT_URI,
    response_type: 'code',
    scope: 'openid email profile',
    state,
    access_type: 'online',
    prompt: 'select_account',
  });

  res.redirect(`https://accounts.google.com/o/oauth2/v2/auth?${params.toString()}`);
});

authRouter.get('/auth/google/callback', async (req, res) => {
  const failure = (reason) => res.redirect(`/login.html?error=${reason}`);

  if (!googleClient || !process.env.GOOGLE_CLIENT_SECRET || !process.env.GOOGLE_REDIRECT_URI) {
    return failure('google_not_configured');
  }

  const { code, state } = req.query;
  const expectedState = req.cookies?.[OAUTH_STATE_COOKIE];
  res.clearCookie(OAUTH_STATE_COOKIE);

  if (!code || !state || !expectedState || state !== expectedState) {
    return failure('google_state_mismatch');
  }

  let idToken;
  try {
    const tokenRes = await fetch('https://oauth2.googleapis.com/token', {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({
        code,
        client_id: process.env.GOOGLE_CLIENT_ID,
        client_secret: process.env.GOOGLE_CLIENT_SECRET,
        redirect_uri: process.env.GOOGLE_REDIRECT_URI,
        grant_type: 'authorization_code',
      }),
    });
    const tokenBody = await tokenRes.json();
    if (!tokenRes.ok || !tokenBody.id_token) {
      console.error('Google token exchange failed:', tokenBody);
      return failure('google_token_exchange_failed');
    }
    idToken = tokenBody.id_token;
  } catch (err) {
    console.error('Google token exchange request failed:', err);
    return failure('google_token_exchange_failed');
  }

  let payload;
  try {
    const ticket = await googleClient.verifyIdToken({
      idToken,
      audience: process.env.GOOGLE_CLIENT_ID,
    });
    payload = ticket.getPayload();
  } catch (err) {
    console.error('Google id_token verification failed:', err);
    return failure('google_token_invalid');
  }

  if (!payload?.email || !payload.email_verified) {
    return failure('google_email_unverified');
  }

  const normalizedEmail = payload.email.trim().toLowerCase();

  // Find by Google subject first (returning user), then by email (an
  // existing password account signing in with Google for the first time —
  // link it rather than creating a duplicate), then create fresh.
  let account = (
    await pool.query('SELECT id, email, tier FROM accounts WHERE google_sub = $1', [payload.sub])
  ).rows[0];

  if (!account) {
    const byEmail = (
      await pool.query('SELECT id, email, tier FROM accounts WHERE email = $1', [normalizedEmail])
    ).rows[0];

    if (byEmail) {
      // Reaching this callback at all means they came through the gated
      // "Continue with Google" button on our signup/login pages, which
      // requires checking the terms box first — so this counts as
      // acceptance too, same as the password signup path. Only backfills
      // it if it was never set. email_verified is set unconditionally
      // (not COALESCE'd) — Google just reconfirmed payload.email_verified
      // above, which is at least as strong as our own token flow, so this
      // also clears a password-signup account's unverified state if it
      // links to Google later.
      await pool.query(
        `UPDATE accounts
            SET google_sub = $1,
                terms_accepted_at = COALESCE(terms_accepted_at, now()),
                email_verified = true
          WHERE id = $2`,
        [payload.sub, byEmail.id],
      );
      account = byEmail;
    } else {
      // Google accounts skip our own email-verification flow entirely —
      // payload.email_verified (checked above) is already a stronger
      // guarantee than the emailed-link token this codebase uses for
      // password signups.
      const inserted = await pool.query(
        `INSERT INTO accounts (email, google_sub, tier, terms_accepted_at, email_verified)
         VALUES ($1, $2, 'free', now(), true)
         RETURNING id, email, tier`,
        [normalizedEmail, payload.sub],
      );
      account = inserted.rows[0];
    }
  }

  const sessionId = await createSession(account.id);
  setSessionCookie(res, sessionId);
  res.redirect('/dashboard.html');
});

// --- Email verification ---

// Called from verify-email.html with the token out of the emailed link.
// No auth required — the token itself (single-use, 48h expiry) is the
// credential; this lets a verification link work even in a browser that
// isn't currently logged in as that account.
authRouter.post('/auth/verify-email', async (req, res) => {
  const { token } = req.body ?? {};
  const result = await verifyEmailToken(token);

  if (result === 'verified') return res.json({ ok: true });
  if (result === 'expired') {
    return res
      .status(410)
      .json({ error: 'That link has expired — log in and resend a new one from your dashboard.' });
  }
  return res.status(400).json({ error: 'That verification link is invalid.' });
});

// Resend from the dashboard, for whenever the signup-time email didn't
// land (or was never sent — SMTP not configured yet, etc).
authRouter.post('/auth/resend-verification', requireAuth, async (req, res) => {
  const result = await sendVerificationEmail(req.account);

  if (result === 'already_verified') {
    return res.status(409).json({ error: 'Your email is already verified.' });
  }
  if (result === 'cooldown') {
    return res
      .status(429)
      .json({ error: 'A verification email was just sent — check your inbox before requesting another.' });
  }
  res.json({ ok: true });
});

// --- Password reset ---

// Called from reset-password.html with the token out of the emailed link
// (issued by an admin — see routes/admin.js and lib/password-reset.js).
// No auth required, same reasoning as /auth/verify-email: the single-use,
// expiring token is the credential. Success logs out every existing session
// for the account, so they sign in fresh with the new password.
authRouter.post('/auth/reset-password', async (req, res) => {
  const { token, password } = req.body ?? {};
  const result = await resetPasswordWithToken(token, password);

  if (result === 'reset') return res.json({ ok: true });
  if (result === 'weak_password') {
    return res.status(400).json({ error: 'Password must be at least 8 characters.' });
  }
  if (result === 'expired') {
    return res
      .status(410)
      .json({ error: 'That reset link has expired — contact support for a new one.' });
  }
  return res.status(400).json({ error: 'That reset link is invalid or has already been used.' });
});

authRouter.post('/logout', requireAuth, async (req, res) => {
  const sessionId = req.cookies?.[SESSION_COOKIE];
  if (sessionId) {
    await pool.query('DELETE FROM sessions WHERE id = $1', [sessionId]);
  }
  res.clearCookie(SESSION_COOKIE);
  res.json({ ok: true });
});
