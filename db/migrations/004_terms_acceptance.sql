-- Records when (if ever) an account accepted the Terms of Service, as an
-- audit trail for the liability/data-loss disclaimers in apps/web/terms.html.

ALTER TABLE accounts ADD COLUMN terms_accepted_at TIMESTAMPTZ;
