-- Standalone has one explicit ownership scope. Additional scopes are a later feature.
CREATE TABLE workspaces (
    workspace_id text PRIMARY KEY CHECK (workspace_id = 'local')
);
INSERT INTO workspaces VALUES ('local');
ALTER TABLE hosts ADD COLUMN workspace_id text NOT NULL DEFAULT 'local' REFERENCES workspaces;
ALTER TABLE devices ADD COLUMN workspace_id text NOT NULL DEFAULT 'local' REFERENCES workspaces;
ALTER TABLE hosts ADD CONSTRAINT hosts_workspace_identity UNIQUE (workspace_id, host_id);
ALTER TABLE devices ADD CONSTRAINT devices_workspace_identity UNIQUE (workspace_id, device_id);
ALTER TABLE grants ADD COLUMN workspace_id text NOT NULL DEFAULT 'local' REFERENCES workspaces;
ALTER TABLE grants ADD CONSTRAINT grants_host_workspace FOREIGN KEY (workspace_id, host_id) REFERENCES hosts(workspace_id, host_id);
ALTER TABLE grants ADD CONSTRAINT grants_device_workspace FOREIGN KEY (workspace_id, device_id) REFERENCES devices(workspace_id, device_id);
-- Multiple Host enrollments must not silently replace a device's earlier credentials.
CREATE TABLE device_credentials (
    auth_hash bytea PRIMARY KEY,
    device_id text NOT NULL REFERENCES devices ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO device_credentials(auth_hash, device_id) SELECT auth_hash, device_id FROM devices;
CREATE TABLE pairing_operations (
    claim_id text PRIMARY KEY,
    token_hash bytea NOT NULL REFERENCES pairing_tokens ON DELETE CASCADE,
    host_id text NOT NULL REFERENCES hosts,
    device_id text NOT NULL,
    hello_hash bytea NOT NULL,
    hello_response bytea NOT NULL,
    expires_at timestamptz NOT NULL,
    finish_hash bytea,
    finish_response bytea,
    commit_request_id text REFERENCES requests,
    grant_generation bigint,
    auth_hash bytea,
    CHECK ((finish_hash IS NULL AND finish_response IS NULL AND commit_request_id IS NULL AND grant_generation IS NULL AND auth_hash IS NULL)
        OR (finish_hash IS NOT NULL AND finish_response IS NOT NULL AND commit_request_id IS NOT NULL AND grant_generation IS NOT NULL AND auth_hash IS NOT NULL))
);
CREATE INDEX pairing_operations_expiry ON pairing_operations(expires_at);
