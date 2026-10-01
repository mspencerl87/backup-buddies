import { Router } from 'express';
import crypto from 'crypto';
import bcrypt from 'bcryptjs';
import { OAuth2Client } from 'google-auth-library';
import { pool } from '../db/pool.js';
import { SESSION_COOKIE, requireAuth } from '../middleware/require-auth.js';

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
  const { email, password } = req.body ?? {};

  if (typeof email !== 'string' || typeof password !== 'string') {
    return res.status(400).json({ error: 'email and password are required' });
  }
  if (password.length < 8) {
    return res.status(400).json({ error: 'password must be at least 8 characters' });
  }

  const normalizedEmail = email.trim().toLowerCase();
  const passwordHash = await bcrypt.hash(password, 12);

  try {
    const result = await pool.query(
      `INSERT INTO accounts (email, password_hash, tier)
       VALUES ($1, $2, 'free')
       RETURNING id, email, tier, created_at`,
      [normalizedEmail, passwordHash],
    );

    const account = result.rows[0];
    const sessionId = await createSession(account.id);
    setSessionCookie(res, sessionId);

    res.status(201).json({ account });
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
      await pool.query('UPDATE accounts SET google_sub = $1 WHERE id = $2', [payload.sub, byEmail.id]);
      account = byEmail;
    } else {
      const inserted = await pool.query(
        `INSERT INTO accounts (email, google_sub, tier)
         VALUES ($1, $2, 'free')
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

authRouter.post('/logout', requireAuth, async (req, res) => {
  const sessionId = req.cookies?.[SESSION_COOKIE];
  if (sessionId) {
    await pool.query('DELETE FROM sessions WHERE id = $1', [sessionId]);
  }
  res.clearCookie(SESSION_COOKIE);
  res.json({ ok: true });
});
