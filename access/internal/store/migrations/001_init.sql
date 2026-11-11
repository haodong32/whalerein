-- WhaleRein Access first schema.
--
-- Exactly five tables: hosts, devices, grants, pairing_tokens, requests.
-- There is deliberately no users, queue, presence, migration metadata or
-- control-plane table. Migrations are idempotent and guarded by an advisory
-- lock; the schema version is the shape of these tables.

CREATE TABLE IF NOT EXISTS hosts (
    host_id          text PRIMARY KEY,
    fingerprint      text NOT NULL DEFAULT '',
    protocol_version integer NOT NULL DEFAULT 0,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS devices (
    device_id              text PRIMARY KEY,
    public_key_fingerprint text NOT NULL DEFAULT '',
    auth_hash              bytea NOT NULL,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT devices_auth_hash_key UNIQUE (auth_hash)
);

CREATE TABLE IF NOT EXISTS grants (
    host_id    text NOT NULL REFERENCES hosts (host_id) ON DELETE RESTRICT,
    device_id  text NOT NULL REFERENCES devices (device_id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    replayed_at timestamptz,
    PRIMARY KEY (host_id, device_id)
);

CREATE TABLE IF NOT EXISTS pairing_tokens (
    token_hash    bytea PRIMARY KEY,
    host_id       text NOT NULL REFERENCES hosts (host_id) ON DELETE RESTRICT,
    expires_at    timestamptz NOT NULL,
    used_at       timestamptz,
    failure_count integer NOT NULL DEFAULT 0,
    last_failure_at timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT pairing_tokens_failure_count_check CHECK (failure_count >= 0)
);

CREATE TABLE IF NOT EXISTS requests (
    request_id        text PRIMARY KEY,
    host_id           text NOT NULL REFERENCES hosts (host_id) ON DELETE RESTRICT,
    device_id         text NOT NULL REFERENCES devices (device_id) ON DELETE RESTRICT,
    command_id        text NOT NULL DEFAULT '',
    payload_digest    bytea NOT NULL DEFAULT ''::bytea,
    kind              integer NOT NULL,
    protocol_version  integer NOT NULL DEFAULT 0,
    status            text NOT NULL,
    request_body      bytea NOT NULL DEFAULT ''::bytea,
    result_body       bytea NOT NULL DEFAULT ''::bytea,
    error_code        integer NOT NULL DEFAULT 0,
    error_message     text NOT NULL DEFAULT '',
    channel_id        text NOT NULL DEFAULT '',
    trace_id          text NOT NULL DEFAULT '',
    expires_at        timestamptz NOT NULL,
    forward_state     text NOT NULL DEFAULT 'pending',
    forward_after     timestamptz NOT NULL DEFAULT now(),
    forward_attempts  integer NOT NULL DEFAULT 0,
    published_once    boolean NOT NULL DEFAULT false,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT requests_status_check CHECK (status IN (
        'received', 'host_offline', 'rejected', 'host_accepted',
        'succeeded', 'failed', 'unknown'
    )),
    CONSTRAINT requests_forward_state_check CHECK (forward_state IN ('none', 'pending', 'published')),
    CONSTRAINT requests_forward_attempts_check CHECK (forward_attempts >= 0),
    CONSTRAINT requests_protocol_version_check CHECK (protocol_version >= 0)
);

-- One command per (host, device). Handshakes and queries keep command_id empty
-- and are therefore not constrained.
CREATE UNIQUE INDEX IF NOT EXISTS requests_command_unique
    ON requests (host_id, device_id, command_id)
    WHERE command_id <> '';

-- Due forwarding candidates. The partial index keeps the scan small even when
-- most request rows are terminal.
CREATE INDEX IF NOT EXISTS requests_forward_due_idx
    ON requests (forward_after)
    WHERE forward_state = 'pending';

-- Expired request bodies are cleared, then the large bytea columns stay empty.
CREATE INDEX IF NOT EXISTS requests_expiry_idx ON requests (expires_at);

-- Pending revocation replay per host.
CREATE INDEX IF NOT EXISTS grants_pending_revocation_idx
    ON grants (host_id)
    WHERE revoked_at IS NOT NULL AND replayed_at IS NULL;

-- Device-wide revocation scans every grant for one device.
CREATE INDEX IF NOT EXISTS grants_device_idx ON grants (device_id);

-- Invitation expiry cleanup.
CREATE INDEX IF NOT EXISTS pairing_tokens_expiry_idx ON pairing_tokens (expires_at);
