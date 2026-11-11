-- A grant incarnation survives delayed notices without relying on endpoint clocks.
ALTER TABLE grants ADD COLUMN IF NOT EXISTS grant_generation bigint NOT NULL DEFAULT 0
    CHECK (grant_generation >= 0);
