-- Query-lane ciphertext binding. A durable object reserved for a transient
-- query records the admitted request, the Broker-stamped connection instance
-- and the managed enrollment generation alongside the existing grant fence.
-- Command and attachment objects keep the older shape: an empty request_id and
-- a NULL enrollment mean "legacy scope" and never receive the query checks.
--
-- issued_at records the one moment a query result reference became releasable
-- to its waiter. An unissued query object is never fetchable; an issued one
-- stays readable within the original deadline and current grant even after an
-- Access restart.
ALTER TABLE temporary_ciphertext_objects
    ADD COLUMN IF NOT EXISTS enrollment_generation bigint,
    ADD COLUMN IF NOT EXISTS connection_ref text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS issued_at timestamptz;

ALTER TABLE temporary_ciphertext_objects
    DROP CONSTRAINT IF EXISTS temporary_ciphertext_objects_issue_check;
ALTER TABLE temporary_ciphertext_objects
    ADD CONSTRAINT temporary_ciphertext_objects_issue_check
        CHECK (issued_at IS NULL OR state = 'ready');

ALTER TABLE temporary_ciphertext_objects
    DROP CONSTRAINT IF EXISTS temporary_ciphertext_objects_enrollment_check;
ALTER TABLE temporary_ciphertext_objects
    ADD CONSTRAINT temporary_ciphertext_objects_enrollment_check
        CHECK (enrollment_generation IS NULL OR enrollment_generation >= 0);

-- Query-scoped ciphertext is looked up by its original admitted request.
CREATE INDEX IF NOT EXISTS temporary_ciphertext_objects_query_scope_idx
    ON temporary_ciphertext_objects (host_id, device_id, request_id, purpose)
    WHERE request_id <> '' AND state IN ('reserved', 'ready');
