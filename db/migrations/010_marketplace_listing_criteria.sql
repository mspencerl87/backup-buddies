-- Self-reported matching criteria for public "Find a buddy" listings, so
-- someone browsing can judge fit beyond just pledge size — e.g. skip a
-- buddy on the other side of the world, or one whose upload speed won't
-- keep up. These are all optional, self-reported at invite-creation time,
-- and shown on the listing (not enforced as filters, unlike pledged_bytes
-- — see routes/invites.js for why size is matched exactly but these
-- aren't).
--
-- always_on and relay_only are self-reported for now rather than derived
-- from actual telemetry (devices.last_seen_at exists, but there's no
-- per-transfer direct-vs-relay record centrally logged — only the client's
-- own local dashboard shows that, per-restore, and never reports it back).
-- Worth revisiting if self-reports turn out to be unreliable in practice.
ALTER TABLE invites ADD COLUMN country TEXT;
ALTER TABLE invites ADD COLUMN speed_tier TEXT;
  -- 'under_10' | '10_50' | '50_200' | '200_plus' — see
  -- apps/web/pledge-options.js SPEED_TIER_OPTIONS, which the API validates
  -- new invites against.
ALTER TABLE invites ADD COLUMN always_on BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE invites ADD COLUMN relay_only BOOLEAN NOT NULL DEFAULT false;

CREATE INDEX idx_invites_public_country ON invites (country)
  WHERE is_public = true AND status = 'pending';
