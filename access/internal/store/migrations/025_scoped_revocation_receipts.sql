-- Scoped revocation evidence.
--
-- A pending receipt freezes the grant and device-revocation generations it was
-- created for, and its enforced row records the exact Broker connection
-- instance the acknowledgement applied to. The connection epoch is the
-- broker-process epoch string (hex), not a numeric counter. These columns are
-- evidence only: they never authorize traffic, and an absent or already
-- replaced connection still counts as enforced for its old scope.
ALTER TABLE external_device_revocations
    ADD COLUMN IF NOT EXISTS grant_generation bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS revocation_generation bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS device_revocation_generation bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS enforced_connection_epoch text,
    ADD COLUMN IF NOT EXISTS enforced_connection_id bigint;

ALTER TABLE external_removals
    ADD COLUMN IF NOT EXISTS enforced_connection_epoch text,
    ADD COLUMN IF NOT EXISTS enforced_connection_id bigint;
