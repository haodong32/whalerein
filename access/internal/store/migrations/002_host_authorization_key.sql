-- Ed25519 public key registered by a host on its authenticated MQTT session.
-- The first non-null value is kept. This does not add a table.

ALTER TABLE hosts ADD COLUMN IF NOT EXISTS authorization_public_key bytea;
