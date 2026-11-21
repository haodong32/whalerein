-- External operation admission and removal enforcement.
--
-- An operation is admitted in its own committed transaction before any
-- mutation transaction runs. The admitted row records the authorization
-- deadline and the expected binding, so a mutation can re-check both after it
-- acquires the Host and operation locks. A mutation that rolls back leaves the
-- admitted row visible instead of an invisible success.
--
-- The enrollment keeps the credential issuance identity beside the broker
-- credential hash. A removal row freezes the target and the exact revoked
-- grant generations, so a retry after the database commit or a lost broker
-- acknowledgement can re-issue the same RevokeHost denials.

ALTER TABLE external_operations
    ADD COLUMN IF NOT EXISTS authorization_expires_at timestamptz,
    ADD COLUMN IF NOT EXISTS expected_workspace_id text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS expected_enrollment_revision text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS admitted_at timestamptz,
    ADD COLUMN IF NOT EXISTS reason text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS credential_issued boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS credential_generation bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS credential_operation_id text NOT NULL DEFAULT '';

UPDATE external_operations
SET outcome = CASE WHEN state = 'operation_committed' THEN 'committed' ELSE 'pending' END
WHERE outcome = '';

ALTER TABLE external_enrollments
    ADD COLUMN IF NOT EXISTS current_credential_operation_id text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS credential_generation bigint NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS external_operations_host
    ON external_operations (host_id, operation_id);

CREATE TABLE IF NOT EXISTS external_removals (
    removal_operation_id     text PRIMARY KEY,
    host_id                  text NOT NULL REFERENCES external_enrollments (host_id),
    workspace_id             text NOT NULL,
    enrollment_operation_id  text NOT NULL,
    enrollment_revision      text NOT NULL,
    disabled_at              timestamptz NOT NULL,
    revocations              jsonb NOT NULL DEFAULT '{}'::jsonb,
    enforced_at              timestamptz,
    created_at               timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS external_removals_host
    ON external_removals (host_id, disabled_at DESC);
