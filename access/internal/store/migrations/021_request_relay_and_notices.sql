-- Relay epoch, per-request representation and durable notice intent.
--
-- relay_version is the coordinated contract epoch admitted for the row. Rows
-- created before the cut carry zero and are never admitted again.
--
-- body_reference/result_reference hold the serialized opaque PayloadReference
-- for the representation received or accepted on the wire. Access never
-- decrypts or inspects the referenced ciphertext; only routing metadata is
-- stored here.
--
-- A notice intent is written by the same transaction that applies the
-- authoritative status change. notice_status is the latest state to publish:
-- a later transition overwrites a pending intermediate one, while terminal
-- states remain pending until a bounded worker records publication.

ALTER TABLE requests
    ADD COLUMN IF NOT EXISTS relay_version integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS body_reference bytea NOT NULL DEFAULT ''::bytea,
    ADD COLUMN IF NOT EXISTS result_reference bytea NOT NULL DEFAULT ''::bytea,
    ADD COLUMN IF NOT EXISTS notice_pending boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS notice_attempts integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS notice_next_attempt timestamptz,
    ADD COLUMN IF NOT EXISTS notice_published_at timestamptz,
    ADD COLUMN IF NOT EXISTS notice_status text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS notice_updated_at timestamptz;

ALTER TABLE requests
    ADD CONSTRAINT requests_relay_version_check
        CHECK (relay_version IN (0, 3));

ALTER TABLE requests
    ADD CONSTRAINT requests_notice_attempts_check
        CHECK (notice_attempts >= 0);

-- One bounded claim predicate serves the notice worker. The partial index
-- keeps a large history of published rows out of the due scan.
CREATE INDEX IF NOT EXISTS requests_notice_due_idx
    ON requests (notice_next_attempt, notice_updated_at, request_id)
    WHERE notice_pending;
