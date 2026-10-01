import { Router } from 'express';
import { pool } from '../db/pool.js';

export const healthRouter = Router();

// Liveness: process is up, says nothing about dependencies
healthRouter.get('/healthz', (req, res) => {
  res.json({ status: 'ok' });
});

// Readiness: also confirms the DB connection actually works, so a load
// balancer or deploy script can tell "container started" apart from
// "container can actually serve requests"
healthRouter.get('/readyz', async (req, res) => {
  try {
    await pool.query('SELECT 1');
    res.json({ status: 'ok' });
  } catch (err) {
    res.status(503).json({ status: 'error', detail: 'database unreachable' });
  }
});
