-- Existing rows may predate durable forwarding-attempt accounting and cannot
-- prove that a current preflight rejection is the first publication attempt.
ALTER TABLE requests ADD COLUMN IF NOT EXISTS forward_provenance_known boolean NOT NULL DEFAULT false;
