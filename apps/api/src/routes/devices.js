import { Router } from 'express';
import crypto from 'node:crypto';
import { pool } from '../db/pool.js';
import { requireAuth } from '../middleware/require-auth.js';
import { requireDeviceAuth } from '../middleware/require-device-auth.js';
import { recordRelayUsage } from '../lib/relay-billing.js';

export const devicesRouter = Router();

// ---- Browser-side (session-authed): managing your own devices ----

// Create a device row and issue it a token. The full token is returned
// exactly once, here — only its hash is persisted (see migration 006).
// The person copies this into the .env file for the Docker client.
devicesRouter.post('/devices', requireAuth, async (req, res) => {
  const { label } = req.body ?? {};

  const token = crypto.randomBytes(32).toString('hex');
  const tokenHash = crypto.createHash('sha256').update(token).digest('hex');
  const tokenLastFour = token.slice(-4);

  const result = await pool.query(
    `INSERT INTO devices (account_id, label, token_hash, token_last_four)
     VALUES ($1, $2, $3, $4)
     RETURNING id, label, created_at, token_last_four`,
    [req.account.id, label || null, tokenHash, tokenLastFour],
  );

  res.status(201).json({ device: result.rows[0], token });
});

devicesRouter.get('/devices', requireAuth, async (req, res) => {
  // client_version via to_jsonb so this query works whether or not
  // migration 015 has been applied yet (a missing column just reads NULL).
  const result = await pool.query(
    `SELECT id, iroh_node_id, label, token_last_four, created_at, last_seen_at,
            to_jsonb(devices) ->> 'client_version' AS client_version
       FROM devices
      WHERE account_id = $1
      ORDER BY created_at`,
    [req.account.id],
  );
  res.json({ devices: result.rows });
});

devicesRouter.delete('/devices/:id', requireAuth, async (req, res) => {
  const result = await pool.query(
    `DELETE FROM devices WHERE id = $1 AND account_id = $2 RETURNING id`,
    [req.params.id, req.account.id],
  );
  if (result.rows.length === 0) {
    return res.status(404).json({ error: 'device not found' });
  }
  res.status(204).end();
});

// ---- Client-side (device-token-authed): the headless agent itself ----

// Clients 0.2.0+ identify themselves as "backup-buddies-client/<version>"
// in the User-Agent (apps/client/src/main.rs). Stored per device so the web
// dashboard can flag devices running an outdated client. Only written when
// it changes, so the 30s poll doesn't turn into a write every time.
const CLIENT_UA = /^backup-buddies-client\/(\d+\.\d+\.\d+[\w.-]*)/;
let clientVersionColumnMissing = false;

async function recordClientVersion(req) {
  const version = CLIENT_UA.exec(req.get('user-agent') || '')?.[1];
  if (!version || version === req.device.client_version || clientVersionColumnMissing) return;
  try {
    await pool.query(`UPDATE devices SET client_version = $1 WHERE id = $2`, [version, req.device.id]);
  } catch (err) {
    // 42703 = undefined column: migration 015 isn't applied yet. Never let
    // that break a device's poll — just stop trying until the next restart.
    if (err.code === '42703') {
      clientVersionColumnMissing = true;
      console.warn('devices.client_version missing — apply db/migrations/015_device_client_version.sql');
      return;
    }
    throw err;
  }
}

// The client calls this on every startup (and periodically) once it has
// generated or loaded its persistent Iroh keypair, so the server always
// has an up-to-date node id to hand to its buddy's client.
devicesRouter.put('/devices/me/node-id', requireDeviceAuth, async (req, res) => {
  const { iroh_node_id } = req.body ?? {};
  if (typeof iroh_node_id !== 'string' || iroh_node_id.length === 0) {
    return res.status(400).json({ error: 'iroh_node_id is required' });
  }

  try {
    await pool.query(
      `UPDATE devices SET iroh_node_id = $1, last_seen_at = now() WHERE id = $2`,
      [iroh_node_id, req.device.id],
    );
  } catch (err) {
    // iroh_node_id is UNIQUE (db/init.sql) — this fires if some other
    // device row already holds this node id (e.g. a stale row from an
    // earlier .env/token mixup). Surface it clearly instead of letting a
    // raw Postgres unique-violation become an opaque 500 that the client
    // then misreports as a bad DEVICE_TOKEN.
    if (err.code === '23505') {
      return res.status(409).json({
        error: 'this iroh_node_id is already registered to a different device',
      });
    }
    throw err;
  }

  res.json({ ok: true });
});

// Who this device is. The client salts its file-encryption key with
// account_id (apps/client/src/crypto.rs), so every device on one account
// derives the same key from the same passphrase, and no two accounts do.
devicesRouter.get('/devices/me', requireDeviceAuth, (req, res) => {
  res.json({ device_id: req.device.id, account_id: req.device.account_id, label: req.device.label });
});

// Tells the client who its buddies' devices are, so it knows which Iroh
// node ids to dial. Doubles as a lightweight heartbeat (updates
// last_seen_at) even when called with no new node id to report.
devicesRouter.get('/devices/me/buddies', requireDeviceAuth, async (req, res) => {
  await pool.query(`UPDATE devices SET last_seen_at = now() WHERE id = $1`, [req.device.id]);
  await recordClientVersion(req);

  // Piggybacks relay-bandwidth reporting on this existing 30s poll rather
  // than a separate endpoint/schedule — see bandwidth.rs on the client.
  // relay_mb is megabytes relayed *since the client's last successful
  // report*, not a lifetime total; the client only advances its own
  // high-water mark once this call succeeds, so a failed poll just means
  // the same delta gets reported again next time.
  const relayMb = Number.parseInt(req.query.relay_mb, 10);
  if (Number.isInteger(relayMb) && relayMb > 0) {
    await recordRelayUsage(req.device.account_id, relayMb);
  }

  // Checked on every poll so a paused account's client finds out within
  // one cycle (~30s), not just the next time it happens to restart — see
  // stripe-webhook.js for what sets this, and relay-billing.js's
  // recordRelayUsage for the server-side backstop that doesn't depend on
  // the client actually honoring this flag.
  const accountResult = await pool.query(`SELECT relay_status FROM accounts WHERE id = $1`, [
    req.device.account_id,
  ]);
  const paused = accountResult.rows[0]?.relay_status !== 'active';

  const result = await pool.query(
    `SELECT devices.id, devices.account_id, devices.iroh_node_id, devices.label,
            buddy_pairings.pledged_bytes
       FROM buddy_pairings
       JOIN devices
         ON devices.account_id = CASE
              WHEN buddy_pairings.account_a_id = $1 THEN buddy_pairings.account_b_id
              ELSE buddy_pairings.account_a_id
            END
      WHERE (buddy_pairings.account_a_id = $1 OR buddy_pairings.account_b_id = $1)
        AND buddy_pairings.status = 'active'
        AND devices.iroh_node_id IS NOT NULL`,
    [req.device.account_id],
  );

  // This device's own label (as set in the dashboard's Devices card) —
  // req.device already has it from requireDeviceAuth's `SELECT *`, no
  // extra query needed. The client surfaces this on its own dashboard's
  // "This Device" line so it reads as something recognizable instead of
  // the raw Iroh node id.
  res.json({ buddy_devices: result.rows, paused, self_label: req.device.label });
});
