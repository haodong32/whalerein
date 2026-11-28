-- The Host-owned grant committed for this pairing outcome. Empty for an outcome
-- committed against an old Host that predates grant IDs, and for legacy rows.
ALTER TABLE pairing_outcomes ADD COLUMN grant_id text;
