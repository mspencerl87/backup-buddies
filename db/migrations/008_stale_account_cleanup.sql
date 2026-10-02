-- Stale-account handling: warn an account's own holder after a month of no
-- device activity, then — if still nothing a month after that — include it
-- in a monthly digest emailed to the admins (ADMIN_EMAILS) so a person
-- decides whether to actually delete it from /admin.html. Nothing in this
-- pipeline deletes or cancels billing on its own; see
-- apps/api/src/jobs/stale-account-cleanup.js.
ALTER TABLE accounts
  ADD COLUMN stale_warning_sent_at TIMESTAMPTZ;

-- Generic "when did this scheduled job last run" tracker, so a monthly job
-- can tell whether it's due without needing a cron library — persisted
-- here (rather than in memory) so it survives restarts/redeploys instead
-- of re-firing (or going silent) around every deploy. One row per job.
CREATE TABLE job_runs (
  job_name     TEXT PRIMARY KEY,
  last_run_at  TIMESTAMPTZ NOT NULL
);
