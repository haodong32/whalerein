-- Minimal authenticated evidence survives invitation and operation retention cleanup.
-- It is intentionally independent of those tables and contains no plaintext token.
CREATE TABLE pairing_outcomes (
    claim_id text PRIMARY KEY,
    token_hash bytea NOT NULL,
    host_id text NOT NULL,
    device_id text NOT NULL,
    finish_hash bytea NOT NULL,
    grant_generation bigint NOT NULL,
    auth_hash bytea NOT NULL,
    commit_request_id text NOT NULL,
    device_public_key bytea,
    protocol_version integer,
    host_confirmed boolean NOT NULL DEFAULT false
);
-- Old responses cannot be decoded by SQL; preserve proof of Access commit but
-- do not infer the Host confirmation or public-key binding.
INSERT INTO pairing_outcomes(claim_id,token_hash,host_id,device_id,finish_hash,grant_generation,auth_hash,commit_request_id)
SELECT claim_id,token_hash,host_id,device_id,finish_hash,grant_generation,auth_hash,commit_request_id
FROM pairing_operations WHERE finish_hash IS NOT NULL;
