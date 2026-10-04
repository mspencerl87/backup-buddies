-- Which client version each device is running, from the User-Agent the
-- client sends on every API call ("backup-buddies-client/0.2.0" — clients
-- older than 0.2.0 don't send one, so they stay NULL). The web dashboard
-- compares it against the latest apps/client/Cargo.toml version and tells
-- the user when an update is available.
--
-- Additive and nullable. The API reads it through to_jsonb(devices) and
-- writes it inside a try/catch, so the code keeps working whether or not
-- this has been applied yet — versions just aren't recorded until it is.
ALTER TABLE devices ADD COLUMN IF NOT EXISTS client_version TEXT;
