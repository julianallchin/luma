-- Clean up any duplicate (venue_id, name) rows that may exist from failed pulls,
-- keeping only the row with the lowest rowid. This is needed for machines where
-- the unique index was missing and duplicate groups were inserted.

DELETE FROM fixture_group_members
  WHERE group_id IN (
    SELECT fg.id FROM fixture_groups fg
    WHERE fg.rowid NOT IN (
      SELECT MIN(rowid) FROM fixture_groups GROUP BY venue_id, name
    )
  );

DELETE FROM fixture_groups
  WHERE rowid NOT IN (
    SELECT MIN(rowid) FROM fixture_groups GROUP BY venue_id, name
  );

-- Ensure the unique index exists (may already exist from prior migration)
CREATE UNIQUE INDEX IF NOT EXISTS idx_fixture_groups_venue_name
    ON fixture_groups(venue_id, name);
