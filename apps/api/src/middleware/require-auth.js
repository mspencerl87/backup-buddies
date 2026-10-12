import { pool } from '../db/pool.js';

const SESSION_COOKIE = 'session_id';
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

// Looks up the session cookie against the sessions table and attaches the
// authenticated account to req.account. Responds 401 if there's no valid,
// unexpired session — callers don't need to check req.account themselves.
export async function requireAuth(req, res, next) {
  const sessionId = req.cookies?.[SESSION_COOKIE];
  if (!sessionId) {
    return res.status(401).json({ error: 'not authenticated' });
  }
  // Session ids are UUIDs (sessions.id); anything else can't match, and
  // would only make Postgres throw (a 500) before saying so.
  if (!UUID_PATTERN.test(sessionId)) {
    return res.status(401).json({ error: 'session expired or invalid' });
  }

  const result = await pool.query(
    `SELECT accounts.*
       FROM sessions
       JOIN accounts ON accounts.id = sessions.account_id
      WHERE sessions.id = $1
        AND sessions.expires_at > now()`,
    [sessionId],
  );

  if (result.rows.length === 0) {
    return res.status(401).json({ error: 'session expired or invalid' });
  }

  req.account = result.rows[0];
  next();
}

export { SESSION_COOKIE };
