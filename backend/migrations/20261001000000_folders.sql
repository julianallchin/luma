-- Folders: a venue's named sets of songs (docs/specs/venue-tabs.md, phase 2).
--
-- A folder holds links to tracks, many to many. A song's scores belong to the
-- song (track + venue), not to a folder, so a song in two folders shows the
-- same scores in both. Deleting a folder deletes its links and nothing else.
--
-- `folder_tracks` keeps its natural key and carries `id` as a generated
-- column, like `venue_node_params`. It carries `venue_id` so the sync rules
-- and the row-level security reach it in one hop, and the composite foreign
-- key keeps that `venue_id` equal to its folder's.
--
-- `track_id` has no local foreign key, for the reason
-- `20260919000000_patterns_without_local_foreign_keys.sql` gives: a venue
-- member downloads every link in the venue, but only the tracks behind the
-- venue's scores. A link to a track the sync rules do not ship would fail the
-- checkpoint forever. Deleting a track deletes its links
-- (`database::local::deletes`).
--
-- Symmetric with supabase/migrations/20261001000000_folders.sql.

CREATE TABLE folders (
    id TEXT PRIMARY KEY,
    uid TEXT,
    venue_id TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_folders_venue ON folders(venue_id);
CREATE UNIQUE INDEX idx_folders_id_venue ON folders(id, venue_id);
CREATE TRIGGER folders_updated_at AFTER UPDATE ON folders FOR EACH ROW
BEGIN UPDATE folders SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;

CREATE TABLE folder_tracks (
    folder_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    uid TEXT,
    venue_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    id TEXT GENERATED ALWAYS AS (folder_id || ':' || track_id) VIRTUAL,
    PRIMARY KEY (folder_id, track_id),
    FOREIGN KEY (folder_id, venue_id) REFERENCES folders(id, venue_id)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX idx_folder_tracks_id ON folder_tracks(id);
CREATE INDEX idx_folder_tracks_venue ON folder_tracks(venue_id);
CREATE INDEX idx_folder_tracks_track ON folder_tracks(track_id);

-- Every venue starts with one folder, "Testing", holding every song it has.
-- The id is derived from the venue, and the Supabase migration derives the
-- same one, so this device's copy and the server's are one row.
INSERT INTO folders (id, uid, venue_id, name)
SELECT id || ':testing', uid, id, 'Testing' FROM venues;

INSERT INTO folder_tracks (folder_id, track_id, uid, venue_id)
SELECT DISTINCT venue.id || ':testing', score.track_id, venue.uid, venue.id
FROM scores score
JOIN venues venue ON venue.id = score.venue_id;
