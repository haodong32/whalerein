-- Receiver capability must survive durable forwarding and Access restarts.
ALTER TABLE requests
    ADD COLUMN IF NOT EXISTS client_transport_version integer NOT NULL DEFAULT 0 CHECK (client_transport_version IN (0, 2));
