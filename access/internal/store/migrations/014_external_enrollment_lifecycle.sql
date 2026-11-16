-- External enrollment lifecycle and Host ownership proofs.
--
-- Readiness records the operation outcome in the same transaction that
-- advances the enrollment, so operation status and Host readiness cannot
-- disagree. A Host ownership proof nonce is consumed once per purpose so a
-- captured status or credential-replacement proof cannot be replayed.

ALTER TABLE external_operations ADD COLUMN IF NOT EXISTS outcome text NOT NULL DEFAULT '';
ALTER TABLE external_operations ADD COLUMN IF NOT EXISTS updated_at timestamptz;

CREATE TABLE IF NOT EXISTS host_proof_nonces (
    purpose    text NOT NULL,
    nonce_hash bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (purpose, nonce_hash)
);

CREATE INDEX IF NOT EXISTS host_proof_nonces_created_at
    ON host_proof_nonces (created_at);
