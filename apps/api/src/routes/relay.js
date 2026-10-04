import crypto from 'node:crypto';
import { Router } from 'express';
import { pool } from '../db/pool.js';

export const relayRouter = Router();

// Called by relay-1 (iroh-relay's `access.http` mode, see
// deploy/relay/config.toml) every time a device opens a connection to it:
// a POST with the connecting device's Iroh endpoint id (hex) in the
// X-Iroh-NodeId header. The relay admits the device only on a 200
// whose body is exactly `true`; anything else, including this API being
// unreachable, denies it.
//
// Admitted: a device registered here (devices.iroh_node_id) whose account
// isn't paused. That keeps the relay closed to anyone who isn't a customer,
// and makes a paused account (failed payment, free relay allowance used up,
// admin hold) unable to relay at all, on top of the client-side `paused`
// flag it already honors.
//
// The relay only checks on connect, not per packet — an already-connected
// device stays connected until it reconnects. The client-side pause covers
// that gap for normal clients; the relay's per-device bandwidth ceiling
// (deploy/relay/config.toml [limits]) bounds what any one device can do.
//
// Hot path: denied devices retry about every 16s, and every device
// reconnects at once whenever the relay restarts. This is one indexed
// lookup (iroh_node_id is UNIQUE) and nothing else.
//
// RELAY_ACCESS_TOKEN must be set to the same value as the relay's
// IROH_RELAY_HTTP_BEARER_TOKEN, so nobody but relay-1 can probe which
// node ids are registered.

const NODE_ID_PATTERN = /^[0-9a-f]{64}$/;

function tokenMatches(header) {
  const expected = process.env.RELAY_ACCESS_TOKEN;
  if (!expected || typeof header !== 'string' || !header.startsWith('Bearer ')) return false;
  const given = Buffer.from(header.slice('Bearer '.length));
  const want = Buffer.from(expected);
  return given.length === want.length && crypto.timingSafeEqual(given, want);
}

relayRouter.post('/relay/access', async (req, res) => {
  if (!process.env.RELAY_ACCESS_TOKEN) {
    console.error('Relay access check called but RELAY_ACCESS_TOKEN is not set — denying.');
    return res.status(503).type('text/plain').send('false');
  }
  if (!tokenMatches(req.get('authorization'))) {
    return res.status(401).type('text/plain').send('false');
  }

  // iroh-relay 1.3.0 sends `X-Iroh-NodeId` — its own docs say
  // `X-Iroh-Endpoint-Id`, but the constant's actual value is the former
  // (verified against the real binary). Accept both, so a future relay
  // version that renames it to match its docs keeps working.
  const nodeId = (req.get('x-iroh-nodeid') || req.get('x-iroh-endpoint-id') || '').trim().toLowerCase();
  if (!NODE_ID_PATTERN.test(nodeId)) {
    return res.status(400).type('text/plain').send('false');
  }

  const result = await pool.query(
    `SELECT accounts.relay_status
       FROM devices
       JOIN accounts ON accounts.id = devices.account_id
      WHERE devices.iroh_node_id = $1`,
    [nodeId],
  );
  const allowed = result.rows[0]?.relay_status === 'active';
  res.status(allowed ? 200 : 403).type('text/plain').send(allowed ? 'true' : 'false');
});
