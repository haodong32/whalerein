-- Node-owned temporary ciphertext metadata. PostgreSQL owns admission, scope,
-- state and the hard expiry; the file itself lives in one dedicated node
-- directory and never appears in the database or in a log line.
--
-- reserved_bytes is the charged capacity (it may include staging overhead and
-- is never smaller than the exact body length). A row is charged from
-- reservation until successful file deletion, so partial, expired and
-- pending-cleanup bytes keep counting against admission. A reserved object is
-- never readable.

CREATE TABLE IF NOT EXISTS temporary_ciphertext_objects (
    object_id                    text PRIMARY KEY,
    node_id                      text NOT NULL,
    owner_id                     text NOT NULL,
    host_id                      text NOT NULL,
    device_id                    text NOT NULL DEFAULT '',
    app_session_id               text NOT NULL DEFAULT '',
    channel_id                   text NOT NULL DEFAULT '',
    request_id                   text NOT NULL DEFAULT '',
    message_id                   text NOT NULL DEFAULT '',
    event_id                     text NOT NULL DEFAULT '',
    purpose                      text NOT NULL,
    part_id                      text NOT NULL DEFAULT '',
    state                        text NOT NULL,
    reserved_bytes               bigint NOT NULL,
    length_bytes                 bigint NOT NULL,
    digest                       bytea NOT NULL,
    expires_at                   timestamptz NOT NULL,
    created_at                   timestamptz NOT NULL,
    updated_at                   timestamptz NOT NULL,
    ready_at                     timestamptz,
    reader_count                 integer NOT NULL DEFAULT 0,
    cleanup_attempts             integer NOT NULL DEFAULT 0,
    next_attempt_at              timestamptz,
    grant_generation             bigint,
    revocation_generation        bigint,
    device_revocation_generation bigint,
    CONSTRAINT temporary_ciphertext_objects_id_check
        CHECK (object_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT temporary_ciphertext_objects_state_check
        CHECK (state IN ('reserved', 'ready', 'expired', 'deleted')),
    CONSTRAINT temporary_ciphertext_objects_bytes_check
        CHECK (reserved_bytes > 0 AND length_bytes > 0 AND length_bytes <= reserved_bytes),
    CONSTRAINT temporary_ciphertext_objects_digest_check
        CHECK (octet_length(digest) = 32),
    CONSTRAINT temporary_ciphertext_objects_counters_check
        CHECK (reader_count >= 0 AND cleanup_attempts >= 0),
    -- A complete scope names one host, one purpose and exactly one
    -- authoritative parent identity. A partially written identity could let a
    -- release match an unrelated object and is rejected at admission.
    CONSTRAINT temporary_ciphertext_objects_scope_check
        CHECK (host_id <> '' AND purpose <> '' AND node_id <> '' AND owner_id <> ''
            AND ((request_id <> '')::int + (message_id <> '')::int + (event_id <> '')::int) = 1),
    CONSTRAINT temporary_ciphertext_objects_session_check
        CHECK (app_session_id = '' OR device_id <> ''),
    -- Grant fence revisions are all-or-nothing, like the request stamp. A
    -- partially written fence would look like zero-valued authority.
    CONSTRAINT temporary_ciphertext_objects_fence_check
        CHECK ((grant_generation IS NULL AND revocation_generation IS NULL
                AND device_revocation_generation IS NULL)
            OR (grant_generation IS NOT NULL AND revocation_generation IS NOT NULL
                AND device_revocation_generation IS NOT NULL)),
    CONSTRAINT temporary_ciphertext_objects_generation_check
        CHECK ((grant_generation IS NULL OR grant_generation >= 0)
            AND (revocation_generation IS NULL OR revocation_generation >= 0)
            AND (device_revocation_generation IS NULL OR device_revocation_generation >= 0)),
    CONSTRAINT temporary_ciphertext_objects_ready_check
        CHECK (state <> 'ready' OR ready_at IS NOT NULL)
);

-- Reader leases make release idempotent: deleting the lease and decrementing
-- the counter happen in one statement, so a repeated close cannot release the
-- same capacity twice.
CREATE TABLE IF NOT EXISTS temporary_ciphertext_leases (
    lease_id   text PRIMARY KEY,
    object_id  text NOT NULL REFERENCES temporary_ciphertext_objects(object_id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL,
    CONSTRAINT temporary_ciphertext_leases_id_check CHECK (lease_id ~ '^[0-9a-f]{32}$')
);

-- Expired/reserved rows still own capacity. The partial indexes cover the
-- admission sums, the active-upload bound, expiry sweeps and cleanup retries.
CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_expiry_idx
    ON temporary_ciphertext_objects (expires_at, object_id)
    WHERE state IN ('reserved', 'ready');

CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_cleanup_idx
    ON temporary_ciphertext_objects (next_attempt_at, object_id)
    WHERE state = 'expired';

CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_active_upload_idx
    ON temporary_ciphertext_objects (node_id)
    WHERE state = 'reserved';

CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_charged_idx
    ON temporary_ciphertext_objects (node_id, host_id)
    WHERE state IN ('reserved', 'ready', 'expired');

CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_scope_idx
    ON temporary_ciphertext_objects (host_id, purpose, request_id, message_id, event_id, part_id)
    WHERE state IN ('reserved', 'ready');

CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_stale_idx
    ON temporary_ciphertext_objects (updated_at, object_id)
    WHERE state = 'reserved';

CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_tombstone_idx
    ON temporary_ciphertext_objects (updated_at, object_id)
    WHERE state = 'deleted';

CREATE INDEX IF NOT EXISTS temporary_ciphertext_leases_object_idx
    ON temporary_ciphertext_leases (object_id);
