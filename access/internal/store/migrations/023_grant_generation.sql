-- Grants belong to one Host enrollment generation. An old grant can never
-- become active against a newer generation even if a future code path forgets
-- to revoke it. 0 is the Standalone generation.
ALTER TABLE grants
    ADD COLUMN IF NOT EXISTS enrollment_generation bigint NOT NULL DEFAULT 0;

UPDATE grants g
SET enrollment_generation = e.generation
FROM external_enrollments e
WHERE e.host_id = g.host_id;

DROP VIEW IF EXISTS grant_authority;

CREATE VIEW grant_authority AS
SELECT g.*,
    g.revoked_at IS NULL
    AND (
        g.enrollment_generation = 0
        OR EXISTS (
            SELECT 1 FROM external_enrollments e
            WHERE e.host_id = g.host_id
              AND e.generation = g.enrollment_generation
              AND e.state = 'enrollment_ready'
              AND e.disabled_at IS NULL)
    )
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
