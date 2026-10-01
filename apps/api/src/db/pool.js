import pg from 'pg';

const { Pool } = pg;

export const pool = new Pool({
  connectionString: process.env.DATABASE_URL,
});

pool.on('error', (err) => {
  // A background idle-client error should never crash the process
  console.error('Unexpected Postgres pool error', err);
});
