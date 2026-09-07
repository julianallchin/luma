-- Re-add unique index on (venue_id, name) for fixture_groups.
-- This was present in the drop_fixture_remote_id_unique migration but lost
-- during the uuid_everywhere migration. Required for ON CONFLICT(venue_id, name)
-- in pull_venue_groups to work correctly.
CREATE UNIQUE INDEX IF NOT EXISTS idx_fixture_groups_venue_name
    ON fixture_groups(venue_id, name);
