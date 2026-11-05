-- One binding per control and input: learning the same input for the same
-- control again changes that binding rather than adding a second.
DELETE FROM control_bindings
WHERE rowid NOT IN (
    SELECT MIN(rowid) FROM control_bindings GROUP BY venue_id, control, address_json
);
CREATE UNIQUE INDEX idx_control_bindings_input
    ON control_bindings(venue_id, control, address_json);
