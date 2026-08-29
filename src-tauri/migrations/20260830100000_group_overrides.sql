-- Manual edits on top of derived groups.
--
-- `src-tauri/src/services/group_derivation.rs` derives the whole group tree
-- from the rig: role from the fixture definition, rows from the structure
-- fixtures hang on, position splits within a role. Derivation is a pure
-- function of the venue, so it has nowhere to keep a rename.
--
-- This table is that nowhere. A row here **names** a derived node — it does not
-- replace it. "A touched node is never re-derived" is about the node's identity:
-- its label and where it hangs stop following the rule. Its *membership* keeps
-- following the rule, which is what makes renaming a group and then hanging
-- four more pars on that truss file the new pars under the name you gave it.
--
-- `group_id` is the derived id, `derive_groups`'s hash of the venue and the
-- derivation path, so it is stable across re-derivation by construction: an
-- override outlives adding one more par to the rig. `path` is stored beside it
-- so a human reading the table can see which set was touched.
--
-- A row whose path no longer derives is inert rather than orphaned: the patch
-- has nothing to patch. Take the truss down and the rename goes quiet; put it
-- back and the rename comes back with it.
--
-- Local-only, exactly like the venue graph: there is no Supabase counterpart
-- and nothing registers it in `src/sync/registry.rs`.

CREATE TABLE fixture_group_overrides (
    -- `derived_id(venue_id, path)`. Not a foreign key: the thing it names is
    -- derived, not stored, and a constraint against a row that does not exist
    -- is a constraint that cannot be written.
    group_id TEXT PRIMARY KEY,

    venue_id TEXT NOT NULL,

    -- The derivation path, '/'-joined: 'spots/left wing/top'.
    path TEXT NOT NULL,

    -- Rename. NULL keeps the derived label.
    label TEXT,

    -- Move: the group id this node now hangs under. NULL means "where
    -- derivation put it"; the empty string means the top level.
    parent_id TEXT,

    -- Merge: the group id this node's fixtures are counted under instead. The
    -- node itself stops being shown — saying *where* it went is what a plain
    -- "hidden" flag could not, and it is what lets the merge be undone by
    -- deleting one row.
    merged_into TEXT,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,

    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

CREATE INDEX idx_fixture_group_overrides_venue ON fixture_group_overrides(venue_id);

CREATE TRIGGER fixture_group_overrides_updated_at
AFTER UPDATE ON fixture_group_overrides FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE fixture_group_overrides
        SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
            version = OLD.version + 1,
            synced_at = NULL
        WHERE group_id = OLD.group_id;
    END;

-- Write admission. Same aggregate as every other venue-owned table: owners
-- write, joined members are read-only, remote pull and transaction-local
-- maintenance are the two explicit bypasses (`20260802600000`).

CREATE TRIGGER auth_group_override_insert
BEFORE INSERT ON fixture_group_overrides FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = NEW.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'group override write is not authorized'); END;

CREATE TRIGGER auth_group_override_update
BEFORE UPDATE ON fixture_group_overrides FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND (
      NEW.group_id IS NOT OLD.group_id OR NEW.venue_id IS NOT OLD.venue_id
      OR NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
 )
BEGIN SELECT RAISE(ABORT, 'group override write is not authorized'); END;

CREATE TRIGGER auth_group_override_delete
BEFORE DELETE ON fixture_group_overrides FOR EACH ROW
WHEN COALESCE((
        SELECT armed = 1 AND maintenance = 0 AND remote_writes = 0
        FROM auth_write_admission WHERE singleton = 1
     ), 1)
 AND NOT EXISTS (SELECT 1 FROM auth_venue_access WHERE venue_id = OLD.venue_id AND owner_access = 1)
BEGIN SELECT RAISE(ABORT, 'group override write is not authorized'); END;
