-- A terminal regional fence can arrive before any permission for its session.
CREATE TABLE device_sessions (
    session_id text PRIMARY KEY,
    workspace_id text,
    region_id text,
    device_id text,
    key_fingerprint text,
    authority_revision text,
    credential_hash bytea UNIQUE,
    revision bigint NOT NULL CHECK (revision > 0),
    expires_at timestamptz,
    revoked boolean NOT NULL DEFAULT false
);

ALTER TABLE requests ADD COLUMN device_session_id text REFERENCES device_sessions;
CREATE INDEX requests_device_session_live ON requests(device_session_id)
    WHERE device_session_id IS NOT NULL AND client_lease_state='live';

-- Keep the original session identity after bulky pairing transcripts expire.
CREATE TABLE pairing_device_sessions (
    claim_id text PRIMARY KEY,
    session_id text NOT NULL REFERENCES device_sessions,
    workspace_id text NOT NULL,
    host_id text NOT NULL,
    device_id text NOT NULL,
    expires_at timestamptz NOT NULL
);

-- All Host COMMIT evidence passes through the pairing outcome, including
-- acknowledgments picked up by another Access node or a restarted worker.
CREATE FUNCTION guard_session_pairing_activation() RETURNS trigger LANGUAGE plpgsql AS '
DECLARE
    session_active boolean;
BEGIN
    SELECT (NOT s.revoked AND s.expires_at > clock_timestamp()) INTO session_active
    FROM pairing_device_sessions p JOIN device_sessions s ON s.session_id=p.session_id
    WHERE p.claim_id=NEW.claim_id
    FOR SHARE OF s;
    IF session_active IS FALSE THEN
        RETURN NULL;
    END IF;
    RETURN NEW;
END;
';

CREATE TRIGGER session_pairing_activation
BEFORE UPDATE OF host_confirmed, host_confirmation_verified ON pairing_outcomes
FOR EACH ROW WHEN (NEW.host_confirmed AND NEW.host_confirmation_verified AND NOT OLD.host_confirmation_verified)
EXECUTE FUNCTION guard_session_pairing_activation();
