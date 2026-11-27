-- SQLite schema for one Access node.
-- Generated from the reviewed PostgreSQL migrations; timestamps are Unix milliseconds.
-- This is the single runtime schema: Access opens no PostgreSQL connection.
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;

CREATE TABLE IF NOT EXISTS app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS device_credentials (
    auth_hash BLOB NOT NULL,
    device_id text NOT NULL,
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    PRIMARY KEY (auth_hash)
);

CREATE TABLE IF NOT EXISTS device_sessions (
    session_id text NOT NULL,
    workspace_id text,
    region_id text,
    device_id text,
    key_fingerprint text,
    authority_revision text,
    credential_hash BLOB,
    revision INTEGER NOT NULL,
    expires_at INTEGER,
    revoked INTEGER DEFAULT 0 NOT NULL,
    UNIQUE (credential_hash),
    PRIMARY KEY (session_id)
);

CREATE TABLE IF NOT EXISTS devices (
    device_id text NOT NULL,
    public_key_fingerprint text DEFAULT '',
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    updated_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    revocation_generation INTEGER DEFAULT 0 NOT NULL,
    PRIMARY KEY (device_id)
);

CREATE TABLE IF NOT EXISTS external_device_revocations (
    host_id text NOT NULL,
    device_id text NOT NULL,
    revoked_at INTEGER NOT NULL,
    enforced_at INTEGER,
    generation INTEGER DEFAULT 1 NOT NULL,
    grant_generation INTEGER DEFAULT 0 NOT NULL,
    revocation_generation INTEGER DEFAULT 0 NOT NULL,
    device_revocation_generation INTEGER DEFAULT 0,
    enforced_connection_epoch text,
    enforced_connection_id INTEGER,
    PRIMARY KEY (host_id, device_id, generation)
);

CREATE TABLE IF NOT EXISTS external_enrollments (
    host_id text NOT NULL,
    workspace_id text NOT NULL,
    enrollment_revision text NOT NULL,
    host_signing_public_key BLOB NOT NULL,
    host_noise_public_key BLOB NOT NULL,
    broker_credential_hash BLOB NOT NULL,
    operation_id text NOT NULL,
    state text DEFAULT 'enrollment_pending',
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    disabled_at INTEGER,
    current_credential_operation_id text DEFAULT '',
    credential_generation INTEGER DEFAULT 0 NOT NULL,
    generation INTEGER DEFAULT 1 NOT NULL,
    PRIMARY KEY (host_id)
);

CREATE TABLE IF NOT EXISTS external_operations (
    operation_id text NOT NULL,
    authorization_id text NOT NULL,
    purpose text NOT NULL,
    workspace_id text NOT NULL,
    region_id text NOT NULL,
    host_id text DEFAULT '',
    payload_digest BLOB NOT NULL,
    state text NOT NULL,
    enrollment_revision text DEFAULT '',
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    committed_at INTEGER,
    outcome text DEFAULT '',
    updated_at INTEGER,
    authorization_expires_at INTEGER,
    expected_workspace_id text DEFAULT '',
    expected_enrollment_revision text DEFAULT '',
    admitted_at INTEGER,
    reason text DEFAULT '',
    credential_issued INTEGER DEFAULT 0 NOT NULL,
    credential_generation INTEGER DEFAULT 0 NOT NULL,
    credential_operation_id text DEFAULT '',
    UNIQUE (authorization_id),
    PRIMARY KEY (operation_id)
);

CREATE TABLE IF NOT EXISTS external_removals (
    removal_operation_id text NOT NULL,
    host_id text NOT NULL,
    workspace_id text NOT NULL,
    enrollment_operation_id text NOT NULL,
    enrollment_revision text NOT NULL,
    disabled_at INTEGER NOT NULL,
    revocations TEXT DEFAULT '{}',
    enforced_at INTEGER,
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    generation INTEGER DEFAULT 1 NOT NULL,
    enforced_connection_epoch text,
    enforced_connection_id INTEGER,
    PRIMARY KEY (removal_operation_id)
);

CREATE TABLE IF NOT EXISTS grants (
    host_id text NOT NULL,
    device_id text NOT NULL,
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    updated_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    revoked_at INTEGER,
    replayed_at INTEGER,
    grant_generation INTEGER DEFAULT 0 NOT NULL,
    revocation_generation INTEGER DEFAULT 0 NOT NULL,
    enrollment_generation INTEGER DEFAULT 0 NOT NULL,
    PRIMARY KEY (host_id, device_id)
);

CREATE TABLE IF NOT EXISTS pairing_outcomes (
    claim_id text NOT NULL,
    token_hash BLOB NOT NULL,
    host_id text NOT NULL,
    device_id text NOT NULL,
    finish_hash BLOB NOT NULL,
    grant_generation INTEGER NOT NULL,
    auth_hash BLOB NOT NULL,
    commit_request_id text NOT NULL,
    device_public_key BLOB,
    protocol_version INTEGER,
    host_confirmed INTEGER DEFAULT 0 NOT NULL,
    commit_binding_verified INTEGER DEFAULT 0 NOT NULL,
    host_confirmation_verified INTEGER DEFAULT 0 NOT NULL,
    PRIMARY KEY (claim_id)
);

CREATE TABLE IF NOT EXISTS host_proof_nonces (
    purpose text NOT NULL,
    nonce_hash BLOB NOT NULL,
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    PRIMARY KEY (purpose, nonce_hash)
);

CREATE TABLE IF NOT EXISTS host_signature_replays (
    signature_hash BLOB NOT NULL,
    host_id text NOT NULL,
    purpose text NOT NULL,
    device_id text DEFAULT '',
    signed_at_unix_ms INTEGER NOT NULL,
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    PRIMARY KEY (signature_hash)
);

CREATE TABLE IF NOT EXISTS hosts (
    host_id text NOT NULL,
    fingerprint text DEFAULT '',
    protocol_version INTEGER DEFAULT 0 NOT NULL,
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    updated_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    authorization_public_key BLOB,
    PRIMARY KEY (host_id)
);

CREATE TABLE IF NOT EXISTS pairing_device_sessions (
    claim_id text NOT NULL,
    session_id text NOT NULL,
    workspace_id text NOT NULL,
    host_id text NOT NULL,
    device_id text NOT NULL,
    expires_at INTEGER NOT NULL,
    authority_revision text NOT NULL,
    PRIMARY KEY (claim_id)
);

CREATE TABLE IF NOT EXISTS pairing_operations (
    claim_id text NOT NULL,
    token_hash BLOB NOT NULL,
    host_id text NOT NULL,
    device_id text NOT NULL,
    hello_hash BLOB NOT NULL,
    hello_response BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    finish_hash BLOB,
    finish_response BLOB,
    commit_request_id text,
    grant_generation INTEGER,
    auth_hash BLOB,
    device_generation INTEGER DEFAULT 0 NOT NULL,
    host_generation INTEGER DEFAULT 0 NOT NULL,
    PRIMARY KEY (claim_id)
);

CREATE TABLE IF NOT EXISTS pairing_tokens (
    token_hash BLOB NOT NULL,
    host_id text NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at INTEGER,
    failure_count INTEGER DEFAULT 0 NOT NULL,
    last_failure_at INTEGER,
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    updated_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    PRIMARY KEY (token_hash)
);

CREATE TABLE IF NOT EXISTS requests (
    request_id text NOT NULL,
    host_id text NOT NULL,
    device_id text NOT NULL,
    command_id text DEFAULT '',
    payload_digest BLOB DEFAULT '\x',
    kind INTEGER NOT NULL,
    protocol_version INTEGER DEFAULT 0 NOT NULL,
    status text NOT NULL,
    request_body BLOB DEFAULT '\x',
    result_body BLOB DEFAULT '\x',
    error_code INTEGER DEFAULT 0 NOT NULL,
    error_message text DEFAULT '',
    channel_id text DEFAULT '',
    trace_id text DEFAULT '',
    expires_at INTEGER NOT NULL,
    forward_state text DEFAULT 'pending',
    forward_after INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    forward_attempts INTEGER DEFAULT 0 NOT NULL,
    published_once INTEGER DEFAULT 0 NOT NULL,
    created_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    updated_at INTEGER DEFAULT (unixepoch()*1000) NOT NULL,
    client_transport_version INTEGER DEFAULT 0 NOT NULL,
    bodies_cleared INTEGER DEFAULT 0,
    forward_provenance_known INTEGER DEFAULT 0 NOT NULL,
    client_lease_until INTEGER,
    client_lease_state text DEFAULT 'live',
    cancel_pending INTEGER DEFAULT 0 NOT NULL,
    cancel_published_at INTEGER,
    cancel_attempts INTEGER DEFAULT 0 NOT NULL,
    device_session_id text,
    device_session_authority text,
    grant_generation INTEGER,
    revocation_generation INTEGER,
    device_revocation_generation INTEGER,
    result_limit_bytes INTEGER,
    relay_version INTEGER DEFAULT 0 NOT NULL,
    body_reference BLOB DEFAULT '\x',
    result_reference BLOB DEFAULT '\x',
    notice_pending INTEGER DEFAULT 0 NOT NULL,
    notice_attempts INTEGER DEFAULT 0 NOT NULL,
    notice_next_attempt INTEGER,
    notice_published_at INTEGER,
    notice_status text DEFAULT '',
    notice_updated_at INTEGER,
    PRIMARY KEY (request_id)
);

CREATE TABLE IF NOT EXISTS temporary_ciphertext_leases (
    lease_id text NOT NULL,
    object_id text NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (lease_id)
);

CREATE TABLE IF NOT EXISTS temporary_ciphertext_objects (
    object_id text NOT NULL,
    node_id text NOT NULL,
    owner_id text NOT NULL,
    host_id text NOT NULL,
    device_id text DEFAULT '',
    app_session_id text DEFAULT '',
    channel_id text DEFAULT '',
    request_id text DEFAULT '',
    message_id text DEFAULT '',
    event_id text DEFAULT '',
    purpose text NOT NULL,
    part_id text DEFAULT '',
    state text NOT NULL,
    reserved_bytes INTEGER NOT NULL,
    length_bytes INTEGER NOT NULL,
    digest BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    ready_at INTEGER,
    reader_count INTEGER DEFAULT 0 NOT NULL,
    cleanup_attempts INTEGER DEFAULT 0 NOT NULL,
    next_attempt_at INTEGER,
    grant_generation INTEGER,
    revocation_generation INTEGER,
    device_revocation_generation INTEGER,
    enrollment_generation INTEGER,
    connection_ref text DEFAULT '',
    issued_at INTEGER,
    PRIMARY KEY (object_id)
);

CREATE INDEX IF NOT EXISTS cloud_pairing_outcomes_auth_idx ON pairing_outcomes (device_id, auth_hash) WHERE (length(token_hash) = 33);
CREATE INDEX IF NOT EXISTS cloud_pairing_outcomes_grant_idx ON pairing_outcomes (host_id, device_id, grant_generation) WHERE (length(token_hash) = 33);
CREATE INDEX IF NOT EXISTS external_device_revocations_generation ON external_device_revocations (host_id, generation, device_id);
CREATE INDEX IF NOT EXISTS external_enrollments_workspace ON external_enrollments (workspace_id, state);
CREATE INDEX IF NOT EXISTS external_operations_host ON external_operations (host_id, operation_id);
CREATE INDEX IF NOT EXISTS external_removals_host ON external_removals (host_id, disabled_at DESC);
CREATE INDEX IF NOT EXISTS grants_device_idx ON grants (device_id);
CREATE INDEX IF NOT EXISTS grants_pending_revocation_idx ON grants (host_id) WHERE ((revoked_at IS NOT NULL) AND (replayed_at IS NULL));
CREATE INDEX IF NOT EXISTS host_proof_nonces_created_at ON host_proof_nonces (created_at);
CREATE INDEX IF NOT EXISTS host_signature_replays_created_at ON host_signature_replays (created_at);
CREATE INDEX IF NOT EXISTS pairing_operations_expiry ON pairing_operations (expires_at);
CREATE INDEX IF NOT EXISTS pairing_tokens_expiry_idx ON pairing_tokens (expires_at);
CREATE INDEX IF NOT EXISTS requests_cancel_pending ON requests (client_lease_until, request_id) WHERE cancel_pending;
CREATE INDEX IF NOT EXISTS requests_client_lease_due ON requests (client_lease_until, request_id) WHERE ((client_lease_state = 'live') AND (client_lease_until IS NOT NULL));
CREATE UNIQUE INDEX IF NOT EXISTS requests_command_unique ON requests (host_id, device_id, command_id) WHERE (command_id <> '');
CREATE INDEX IF NOT EXISTS requests_device_session_live ON requests (device_session_id) WHERE ((device_session_id IS NOT NULL) AND (client_lease_state = 'live'));
CREATE INDEX IF NOT EXISTS requests_expiry_idx ON requests (expires_at);
CREATE INDEX IF NOT EXISTS requests_forward_due_idx ON requests (forward_after) WHERE (forward_state = 'pending');
CREATE INDEX IF NOT EXISTS requests_notice_due_idx ON requests (notice_next_attempt, notice_updated_at, request_id) WHERE notice_pending;
CREATE INDEX IF NOT EXISTS requests_query_retention ON requests (expires_at, request_id) WHERE ((command_id = '') AND (kind IN (1, 2)));
CREATE INDEX IF NOT EXISTS requests_retained_result_idx ON requests (expires_at, request_id) WHERE (result_body <> x'');
CREATE INDEX IF NOT EXISTS temporary_ciphertext_leases_object_idx ON temporary_ciphertext_leases (object_id);
CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_active_upload_idx ON temporary_ciphertext_objects (node_id) WHERE (state = 'reserved');
CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_charged_idx ON temporary_ciphertext_objects (node_id, host_id) WHERE (state IN ('reserved', 'ready', 'expired'));
CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_cleanup_idx ON temporary_ciphertext_objects (next_attempt_at, object_id) WHERE (state = 'expired');
CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_expiry_idx ON temporary_ciphertext_objects (expires_at, object_id) WHERE (state IN ('reserved', 'ready'));
CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_query_scope_idx ON temporary_ciphertext_objects (host_id, device_id, request_id, purpose) WHERE ((request_id <> '') AND (state IN ('reserved', 'ready')));
CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_scope_idx ON temporary_ciphertext_objects (host_id, purpose, request_id, message_id, event_id, part_id) WHERE (state IN ('reserved', 'ready'));
CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_stale_idx ON temporary_ciphertext_objects (updated_at, object_id) WHERE (state = 'reserved');
CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_tombstone_idx ON temporary_ciphertext_objects (updated_at, object_id) WHERE (state = 'deleted');


CREATE VIEW IF NOT EXISTS grant_authority AS
SELECT host_id,
    device_id,
    created_at,
    updated_at,
    revoked_at,
    replayed_at,
    grant_generation,
    revocation_generation,
    enrollment_generation,
    ((revoked_at IS NULL) AND ((enrollment_generation = 0) OR (EXISTS ( SELECT 1
           FROM external_enrollments e
          WHERE ((e.host_id = g.host_id) AND (e.generation = g.enrollment_generation) AND (e.state = 'enrollment_ready') AND (e.disabled_at IS NULL))))) AND (NOT (EXISTS ( SELECT 1
           FROM (external_device_revocations r
             JOIN external_enrollments e ON ((e.host_id = r.host_id)))
          WHERE ((r.host_id = g.host_id) AND (r.device_id = g.device_id) AND (r.generation = e.generation))))) AND (NOT (EXISTS ( SELECT 1
           FROM pairing_outcomes p
          WHERE ((p.host_id = g.host_id) AND (p.device_id = g.device_id) AND (p.grant_generation = g.grant_generation) AND (length(p.token_hash) = 33) AND (NOT (p.host_confirmed AND p.host_confirmation_verified AND p.commit_binding_verified))))))) AS active
   FROM grants g;

CREATE TRIGGER IF NOT EXISTS session_pairing_activation
BEFORE UPDATE OF host_confirmed, host_confirmation_verified ON pairing_outcomes
FOR EACH ROW WHEN (new.host_confirmed = 1 AND new.host_confirmation_verified = 1 AND old.host_confirmation_verified = 0)
BEGIN
    SELECT RAISE(ABORT, 'pairing session is inactive')
    WHERE EXISTS (
        SELECT 1 FROM pairing_device_sessions p
        WHERE p.claim_id = NEW.claim_id
          AND NOT EXISTS (
              SELECT 1 FROM device_sessions s
              WHERE s.session_id = p.session_id
                AND s.revoked = 0
                AND s.expires_at > (unixepoch()*1000)
                AND p.authority_revision = s.authority_revision
                AND (COALESCE((SELECT value FROM app_settings WHERE key='whalerein.session_authority'), '') = ''
                     OR s.authority_revision = (SELECT value FROM app_settings WHERE key='whalerein.session_authority'))
          )
    );
END;
