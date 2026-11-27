-- Host-owned display directory. The Host reports these values in HostReady;
-- they are metadata only and never confer authority. Existing rows default to
-- empty strings until the Host next reports readiness.
ALTER TABLE hosts ADD COLUMN IF NOT EXISTS display_name text NOT NULL DEFAULT '';
ALTER TABLE hosts ADD COLUMN IF NOT EXISTS os_family text NOT NULL DEFAULT '';