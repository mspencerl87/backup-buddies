import crypto from 'node:crypto';
import { pool } from '../db/pool.js';

// Authenticates the headless backup-client agent, which has no browser to
// hold a session cookie. The client sends its device token as a bearer
// token; we hash it and look up the matching device row. Only the hash is
// ever stored (see migration 006) — the raw token is shown to the account
// holder exactly once, at device-creation time, in the dashboard.
export async function requireDeviceAuth(req, res, next) {
  const authHeader = req.headers.authorization ?? '';
  const [scheme, token] = authHeader.split(' ');

  if (scheme !== 'Bearer' || !token) {
    return res.status(401).json({ error: 'missing device token' });
  }

  const tokenHash = crypto.createHash('sha256').update(token).digest('hex');
  const result = await pool.query(`SELECT * FROM devices WHERE token_hash = $1`, [tokenHash]);

  if (result.rows.length === 0) {
    return res.status(401).json({ error: 'invalid device token' });
  }

  req.device = result.rows[0];
  next();
}
