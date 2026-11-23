-- Enrollment generations are explicit. A removed generation fences only its
-- own credentials, device revocations and removal rows; a higher generation
-- can re-enable the same stable Host id without reviving old state.

ALTER TABLE external_enrollments
    ADD COLUMN IF NOT EXISTS generation bigint NOT NULL DEFAULT 1;

ALTER TABLE external_removals
    ADD COLUMN IF NOT EXISTS generation bigint NOT NULL DEFAULT 1;

ALTER TABLE external_device_revocations
    ADD COLUMN IF NOT EXISTS generation bigint NOT NULL DEFAULT 1;

ALTER TABLE external_device_revocations
    DROP CONSTRAINT IF EXISTS external_device_revocations_pkey;
ALTER TABLE external_device_revocations
    ADD PRIMARY KEY (host_id, device_id, generation);

CREATE INDEX IF NOT EXISTS external_device_revocations_generation
    ON external_device_revocations (host_id, generation, device_id);

CREATE OR REPLACE VIEW grant_authority AS
SELECT g.*,
    g.revoked_at IS NULL
    AND NOT EXISTS (
        SELECT 1 FROM external_device_revocations r
        JOIN external_enrollments e ON e.host_id = r.host_id
        WHERE r.host_id = g.host_id AND r.device_id = g.device_id
          AND r.generation = e.generation
    )
    AND NOT EXISTS (
        SELECT 1 FROM pairing_outcomes p
        WHERE p.host_id = g.host_id AND p.device_id = g.device_id
          AND p.grant_generation = g.grant_generation
          AND octet_length(p.token_hash) = 33
          AND NOT (p.host_confirmed AND p.host_confirmation_verified AND p.commit_binding_verified)
    ) AS active
FROM grants g;
