-- Global and per-Host revocations invalidate earlier pairing handshakes.
ALTER TABLE devices ADD COLUMN revocation_generation bigint NOT NULL DEFAULT 0 CHECK (revocation_generation >= 0);
ALTER TABLE grants ADD COLUMN revocation_generation bigint NOT NULL DEFAULT 0 CHECK (revocation_generation >= 0);
ALTER TABLE pairing_operations ADD COLUMN device_generation bigint NOT NULL DEFAULT 0;
ALTER TABLE pairing_operations ADD COLUMN host_generation bigint NOT NULL DEFAULT 0;
-- Existing unfinished handshakes have no reliable pre-migration snapshots.
-- Completed receipts retain their existing recovery semantics.
UPDATE pairing_operations SET device_generation = -1, host_generation = -1 WHERE finish_hash IS NULL;
