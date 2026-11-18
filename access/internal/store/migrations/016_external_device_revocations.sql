-- Keep a durable fence even if a Device authorization has not yet produced a
-- grant. The Access line acknowledgement is recorded separately.
CREATE TABLE IF NOT EXISTS external_device_revocations (
    host_id text NOT NULL REFERENCES external_enrollments(host_id),
    device_id text NOT NULL,
    revoked_at timestamptz NOT NULL,
    enforced_at timestamptz,
    PRIMARY KEY (host_id, device_id)
);

CREATE INDEX IF NOT EXISTS cloud_pairing_outcomes_grant_idx
    ON pairing_outcomes (host_id, device_id, grant_generation)
    WHERE octet_length(token_hash) = 33;

CREATE INDEX IF NOT EXISTS cloud_pairing_outcomes_auth_idx
    ON pairing_outcomes (device_id, auth_hash)
    WHERE octet_length(token_hash) = 33;

-- New Cloud grants wait for the exact Host acknowledgement. Standalone
-- grants retain the existing authorization lifecycle.
CREATE OR REPLACE VIEW grant_authority AS
SELECT g.*,
    g.revoked_at IS NULL
    AND NOT EXISTS (
        SELECT 1 FROM external_device_revocations r
        WHERE r.host_id = g.host_id AND r.device_id = g.device_id
    )
    AND NOT EXISTS (
        SELECT 1 FROM pairing_outcomes p
        WHERE p.host_id = g.host_id AND p.device_id = g.device_id
          AND p.grant_generation = g.grant_generation
          AND octet_length(p.token_hash) = 33
          AND NOT (p.host_confirmed AND p.host_confirmation_verified AND p.commit_binding_verified)
    ) AS active
FROM grants g;
