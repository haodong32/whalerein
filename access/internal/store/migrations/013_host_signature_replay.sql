-- Durable one-use guard for Host authorization signatures.
--
-- The process-local replay map is not shared between Access replicas, so a
-- captured invitation or revocation signature could be accepted once per
-- replica. The signature hash is the primary key; each Access instance inserts
-- before acting and only the first insert wins. Rows older than the acceptance
-- window are pruned opportunistically by the same call.
CREATE TABLE IF NOT EXISTS host_signature_replays (
    signature_hash    bytea PRIMARY KEY,
    host_id           text NOT NULL,
    purpose           text NOT NULL,
    device_id         text NOT NULL DEFAULT '',
    signed_at_unix_ms bigint NOT NULL,
    created_at        timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS host_signature_replays_created_at
    ON host_signature_replays (created_at);
