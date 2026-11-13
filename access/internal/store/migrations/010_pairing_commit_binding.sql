-- Existing confirmation flags predate durable COMMIT binding verification.
-- Leave their evidence unverified until the matching retained outbox is checked.
ALTER TABLE pairing_outcomes ADD COLUMN commit_binding_verified boolean NOT NULL DEFAULT false;
ALTER TABLE pairing_outcomes ADD COLUMN host_confirmation_verified boolean NOT NULL DEFAULT false;

-- A NULL value identifies a pre-upgrade row whose body-retention history is
-- unknown. New rows start uncleared; cleanup only changes the marker to true.
ALTER TABLE requests ADD COLUMN bodies_cleared boolean;
ALTER TABLE requests ALTER COLUMN bodies_cleared SET DEFAULT false;
