-- Standalone authorization belongs to this installation. A fixed 'local'
-- workspace table adds no independent authority; external ownership is future work.
ALTER TABLE grants DROP COLUMN workspace_id;
ALTER TABLE hosts DROP COLUMN workspace_id;
ALTER TABLE devices DROP COLUMN workspace_id;
DROP TABLE workspaces;
-- device_credentials already contains migrated and newly issued hashes. Keep
-- identity separate from credentials and remove the unused duplicate hash.
ALTER TABLE devices DROP COLUMN auth_hash;
