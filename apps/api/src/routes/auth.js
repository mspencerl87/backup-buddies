import { Router } from 'express';
import bcrypt from 'bcryptjs';
import { pool } from '../db/pool.js';
import { SESSION_COOKIE, requireAuth } from '../middleware/require-auth.js';

export const authRouter = Router();

const SESSION_TTL_DAYS = 30;

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

authRouter.post('/logout', requireAuth, async (req, res) => {
  const sessionId = req.cookies?.[SESSION_COOKIE];
  if (sessionId) {
    await pool.query('DELETE FROM sessions WHERE id = $1', [sessionId]);
  }
  res.clearCookie(SESSION_COOKIE);
  res.json({ ok: true });
});
