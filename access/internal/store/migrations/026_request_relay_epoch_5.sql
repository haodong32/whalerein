-- The coordinated relay epoch moved from 4 to 5. Durable requests admitted
-- under epoch 5 must be insertable, while historical rows admitted under epoch
-- 4 remain readable for command recovery under the hard cut. Admission rejects
-- retired epochs before this table, so the constraint only guards stored rows.
ALTER TABLE requests DROP CONSTRAINT IF EXISTS requests_relay_version_check;
ALTER TABLE requests ADD CONSTRAINT requests_relay_version_check
    CHECK (relay_version IN (0, 4, 5));
