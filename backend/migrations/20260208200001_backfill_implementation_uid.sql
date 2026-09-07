-- Backfill implementation uid from parent pattern (was never set at creation)
UPDATE implementations SET uid = (
    SELECT p.uid FROM patterns p WHERE p.id = implementations.pattern_id
) WHERE uid IS NULL;
