-- Upload speed tiers go from four to three (under 10 / 10–50 / 50+), now
-- that "Find a buddy" filters on them ("at least 10 Mbps" etc., see GET
-- /invites/public). Above 50 Mbps a home uplink keeps up with a buddy
-- either way, so 50–200 vs. 200+ wasn't worth a filter step of its own.
--
-- The new API only knows the three new values, so apply this on app-1
-- BEFORE pushing the code (docs/DEPLOY.md, section 33). The old API reads
-- speed_tier only for display, so running it early just shows the
-- merged listings without a speed label until the deploy lands.
UPDATE invites SET speed_tier = '50_plus' WHERE speed_tier IN ('50_200', '200_plus');
