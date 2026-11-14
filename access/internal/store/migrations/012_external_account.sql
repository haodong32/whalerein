-- External account mode.
--
-- An enrollment binds one approved Host identity to a workspace. An operation
-- row keeps the immutable bindings of one authorization so a retry cannot
-- change the payload or create a second enrollment. Broker credentials are
-- stored as a hash; the plaintext is returned only by the committing call.

CREATE TABLE IF NOT EXISTS external_operations (
    operation_id        text PRIMARY KEY,
    authorization_id    text NOT NULL UNIQUE,
    purpose             text NOT NULL,
    workspace_id        text NOT NULL,
    region_id           text NOT NULL,
    host_id             text NOT NULL DEFAULT '',
    payload_digest      bytea NOT NULL,
    state               text NOT NULL,
    enrollment_revision text NOT NULL DEFAULT '',
    created_at          timestamptz NOT NULL DEFAULT now(),
    committed_at        timestamptz
);

CREATE TABLE IF NOT EXISTS external_enrollments (
    host_id                  text PRIMARY KEY,
    workspace_id             text NOT NULL,
    enrollment_revision      text NOT NULL,
    host_signing_public_key  bytea NOT NULL,
    host_noise_public_key    bytea NOT NULL,
    broker_credential_hash   bytea NOT NULL,
    operation_id             text NOT NULL REFERENCES external_operations (operation_id),
    state                    text NOT NULL DEFAULT 'enrollment_pending',
    created_at               timestamptz NOT NULL DEFAULT now(),
    disabled_at              timestamptz
);

CREATE INDEX IF NOT EXISTS external_enrollments_workspace
    ON external_enrollments (workspace_id, state);
