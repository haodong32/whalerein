-- Bind each Access-admitted encrypted request to the grant revisions that were
-- active at submission, and to the validated result bound it accepted.
--
-- The columns stay NULL for rows retained from an older build. A NULL stamp is
-- never treated as current: query ciphertext fails closed at release, while a
-- command keeps its metadata-only recovery path. No backfill is possible
-- because the original revisions are gone; fabricating one could authorize
-- stale ciphertext.

ALTER TABLE requests ADD COLUMN IF NOT EXISTS grant_generation bigint;
ALTER TABLE requests ADD COLUMN IF NOT EXISTS revocation_generation bigint;
ALTER TABLE requests ADD COLUMN IF NOT EXISTS device_revocation_generation bigint;
-- Validated raw ciphertext bytes this request may retain and release. The
-- encoded HTTPS bound is derived from it at admission and forwarding.
ALTER TABLE requests ADD COLUMN IF NOT EXISTS result_limit_bytes bigint;

ALTER TABLE requests DROP CONSTRAINT IF EXISTS requests_grant_generation_check;
ALTER TABLE requests ADD CONSTRAINT requests_grant_generation_check
    CHECK (grant_generation IS NULL OR grant_generation >= 0) NOT VALID;

ALTER TABLE requests DROP CONSTRAINT IF EXISTS requests_revocation_generation_check;
ALTER TABLE requests ADD CONSTRAINT requests_revocation_generation_check
    CHECK (revocation_generation IS NULL OR revocation_generation >= 0) NOT VALID;

ALTER TABLE requests DROP CONSTRAINT IF EXISTS requests_device_revocation_generation_check;
ALTER TABLE requests ADD CONSTRAINT requests_device_revocation_generation_check
    CHECK (device_revocation_generation IS NULL OR device_revocation_generation >= 0) NOT VALID;

ALTER TABLE requests DROP CONSTRAINT IF EXISTS requests_result_limit_bytes_check;
ALTER TABLE requests ADD CONSTRAINT requests_result_limit_bytes_check
    CHECK (result_limit_bytes IS NULL OR result_limit_bytes >= 0) NOT VALID;

-- A stamp is all-or-nothing. A partially written stamp would let one missing
-- revision look like a zero-valued authority and silently mismatch forever.
ALTER TABLE requests DROP CONSTRAINT IF EXISTS requests_grant_stamp_complete_check;
ALTER TABLE requests ADD CONSTRAINT requests_grant_stamp_complete_check
    CHECK (
        (grant_generation IS NULL AND revocation_generation IS NULL
            AND device_revocation_generation IS NULL AND result_limit_bytes IS NULL)
        OR (grant_generation IS NOT NULL AND revocation_generation IS NOT NULL
            AND device_revocation_generation IS NOT NULL AND result_limit_bytes IS NOT NULL)
    ) NOT VALID;

-- Result release is a primary-key lookup joined to the current authority, so
-- the fencing decision needs no new lookup index. Retained ciphertext cleanup
-- does benefit from a partial index that skips rows whose result was already
-- cleared; the existing expiry index would otherwise scan them all.
CREATE INDEX IF NOT EXISTS requests_retained_result_idx
    ON requests (expires_at, request_id)
    WHERE result_body <> ''::bytea;
