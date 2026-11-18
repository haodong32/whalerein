-- Queued work keeps the authority incarnation that admitted it. Session
-- renewal after a serving-authority change does not reopen old work.
ALTER TABLE requests ADD COLUMN device_session_authority text;
UPDATE requests r SET device_session_authority=s.authority_revision
    FROM device_sessions s WHERE r.device_session_id=s.session_id;

ALTER TABLE pairing_device_sessions ADD COLUMN authority_revision text;
UPDATE pairing_device_sessions p SET authority_revision=s.authority_revision
    FROM device_sessions s WHERE p.session_id=s.session_id;
ALTER TABLE pairing_device_sessions ALTER COLUMN authority_revision SET NOT NULL;

CREATE OR REPLACE FUNCTION guard_session_pairing_activation() RETURNS trigger LANGUAGE plpgsql AS '
DECLARE
    session_active boolean;
    serving_authority text := current_setting(''whalerein.session_authority'', true);
BEGIN
    SELECT (NOT s.revoked AND s.expires_at > clock_timestamp()
        AND p.authority_revision=s.authority_revision
        AND (COALESCE(serving_authority, '''')='''' OR s.authority_revision=serving_authority))
    INTO session_active
    FROM pairing_device_sessions p JOIN device_sessions s ON s.session_id=p.session_id
    WHERE p.claim_id=NEW.claim_id
    FOR SHARE OF s;
    IF session_active IS FALSE THEN
        RETURN NULL;
    END IF;
    RETURN NEW;
END;
';
