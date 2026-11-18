-- Exact-request HTTP liveness, monotonic expiry and retryable cancellation
-- metadata live on the existing requests row.
--
-- Historical rows keep client_lease_until NULL. A missing deadline is unknown,
-- not elapsed: the sweep only expires rows whose lease deadline was recorded,
-- so an upgrade never implies expiry or native release. Terminal rows stay
-- non-renewable through the status predicate instead of a guessed state.

ALTER TABLE requests ADD COLUMN IF NOT EXISTS client_lease_until timestamptz;
ALTER TABLE requests ADD COLUMN IF NOT EXISTS client_lease_state text NOT NULL DEFAULT 'live';
ALTER TABLE requests ADD COLUMN IF NOT EXISTS cancel_pending boolean NOT NULL DEFAULT false;
ALTER TABLE requests ADD COLUMN IF NOT EXISTS cancel_published_at timestamptz;
ALTER TABLE requests ADD COLUMN IF NOT EXISTS cancel_attempts integer NOT NULL DEFAULT 0;

ALTER TABLE requests DROP CONSTRAINT IF EXISTS requests_client_lease_state_check;
ALTER TABLE requests ADD CONSTRAINT requests_client_lease_state_check
    CHECK (client_lease_state IN ('live', 'expired', 'closed'));

ALTER TABLE requests DROP CONSTRAINT IF EXISTS requests_cancel_attempts_check;
ALTER TABLE requests ADD CONSTRAINT requests_cancel_attempts_check
    CHECK (cancel_attempts >= 0);

-- Bounded sweep and replay candidate queries.
CREATE INDEX IF NOT EXISTS requests_client_lease_due
    ON requests (client_lease_until, request_id)
    WHERE client_lease_state = 'live' AND client_lease_until IS NOT NULL;

CREATE INDEX IF NOT EXISTS requests_cancel_pending
    ON requests (client_lease_until, request_id)
    WHERE cancel_pending;

-- Coordinated cutover: a retained non-terminal command must never be forwarded
-- automatically by the new artifact. A published row may already have executed,
-- so keep its conservative status but stop all forwarding/retry. A row that was
-- still pending was never published and is conservatively unknown. Internal
-- pairing-authorization outbox rows (kind 5) keep their documented retry path.
UPDATE requests
SET status = 'unknown', forward_state = 'none', updated_at = now()
WHERE forward_state = 'pending'
  AND status IN ('received', 'host_accepted')
  AND kind <> 5;

UPDATE requests
SET forward_state = 'none', updated_at = now()
WHERE forward_state = 'published'
  AND status IN ('received', 'host_accepted')
  AND kind <> 5;
