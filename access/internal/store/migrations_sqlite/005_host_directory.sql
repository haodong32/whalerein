-- Host-owned display directory. The Host reports these values in HostReady;
-- they are metadata only and never confer authority. Existing rows default to
-- empty strings until the Host next reports readiness.
ALTER TABLE hosts ADD COLUMN display_name TEXT NOT NULL DEFAULT '';
ALTER TABLE hosts ADD COLUMN os_family TEXT NOT NULL DEFAULT '';