# Post-Cloud Local Database Plan

## Summary

This document outlines the restructuring of Luma's local database architecture in preparation for cloud backup via Supabase. The goals are:

1. **Merge dual-database architecture** — Eliminate the `.luma` project file format and consolidate all data into a single `luma.db` SQLite database.

2. **Decouple implementations from venues** — Implementations become first-class entities with a 1:many relationship to patterns, rather than being venue-specific.

3. **Add cloud sync infrastructure** — Prepare tables with `remote_id`, `version`, and `synced_at` columns for Supabase synchronization.

4. **Rename and restructure annotations** — `track_annotations` becomes `scores`, supporting multiple named scores per track.

5. **MVP0 cloud backup** — Silent, user-naive push sync of all user content to Supabase (no marketplace, no sharing UI, no pull sync).

---

## New Schema Design

### Tables Overview

| Table | Synced | Notes |
|-------|--------|-------|
| `venues` | Yes | NEW — replaces .luma project concept |
| `fixtures` | Yes | Moved from .luma, now has `venue_id` FK |
| `patterns` | Yes | Adds `default_implementation_id` FK |
| `pattern_categories` | Yes | Unchanged |
| `implementations` | Yes | Restructured: own PK, 1:many with patterns |
| `venue_implementation_overrides` | Yes | NEW — per-venue implementation preferences |
| `tracks` | Yes | Unchanged |
| `track_beats` | Yes | Unchanged |
| `track_roots` | Yes | Unchanged |
| `track_waveforms` | Partial | Only `preview_*` columns synced; full regenerated locally |
| `track_stems` | Yes | Metadata synced; files compressed before upload |
| `scores` | Yes | Renamed from `track_annotations`, supports multiple per track |
| `settings` | No | Local-only device settings |

### Cloud Storage (Supabase Storage)

| Bucket | Contents | Notes |
|--------|----------|-------|
| `audio/` | Track audio files | Convert .wav → .mp3 before upload |
| `art/` | Album artwork | Original format |
| `stems/` | Stem audio files | Compress before upload |

---

## Sync Column Pattern

All syncable tables include three columns for cloud synchronization:

| Column | Type | Purpose |
|--------|------|---------|
| `remote_id` | TEXT UNIQUE | UUID identifying this record in Supabase. Set after first successful sync. |
| `version` | INTEGER NOT NULL DEFAULT 1 | Monotonically increasing version number. Incremented on every update via trigger. Used for dirty-checking and future pull sync. |
| `synced_at` | TEXT | ISO timestamp of last successful sync. NULL = never synced. |

### How sync detection works

**Push sync (MVP0):** Query for dirty records using `synced_at IS NULL` (never synced) or by comparing `version` against a stored `synced_version`. After successful push, update both `synced_at` and store the current `version` as `synced_version`.

**Future pull sync:** Server stores `version` for each record. Client can query "give me all records where `version > my_last_known_version`" for efficient delta sync.

### Trigger pattern

Each table has an `AFTER UPDATE` trigger that:
1. Checks `WHEN OLD.version = NEW.version` to prevent infinite loops
2. Increments `version` by 1
3. Updates `updated_at` to current timestamp

This ensures `version` always reflects the true number of mutations, even if `updated_at` timestamps are unreliable.

---

## Complete Schema Definition

### `venues`
```sql
CREATE TABLE venues (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,              -- UUID for cloud sync
    name TEXT NOT NULL,
    description TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT                      -- NULL = never synced
);

CREATE TRIGGER venues_updated_at
    AFTER UPDATE ON venues
    FOR EACH ROW
    WHEN OLD.version = NEW.version      -- Prevent infinite loop
    BEGIN
        UPDATE venues SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;
```

### `fixtures`
```sql
CREATE TABLE fixtures (
    id TEXT PRIMARY KEY,                -- UUID generated on creation
    remote_id TEXT UNIQUE,              -- UUID for cloud sync (can match id)
    venue_id INTEGER NOT NULL,
    universe INTEGER NOT NULL DEFAULT 1,
    address INTEGER NOT NULL,
    num_channels INTEGER NOT NULL,
    manufacturer TEXT NOT NULL,
    model TEXT NOT NULL,
    mode_name TEXT NOT NULL,
    fixture_path TEXT NOT NULL,
    label TEXT,
    pos_x REAL DEFAULT 0.0,
    pos_y REAL DEFAULT 0.0,
    pos_z REAL DEFAULT 0.0,
    rot_x REAL DEFAULT 0.0,
    rot_y REAL DEFAULT 0.0,
    rot_z REAL DEFAULT 0.0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE
);

CREATE INDEX idx_fixtures_venue ON fixtures(venue_id);
CREATE INDEX idx_fixtures_universe ON fixtures(universe);

CREATE TRIGGER fixtures_updated_at
    AFTER UPDATE ON fixtures
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE fixtures SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;
```

### `pattern_categories`
```sql
CREATE TABLE pattern_categories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

CREATE TRIGGER pattern_categories_updated_at
    AFTER UPDATE ON pattern_categories
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE pattern_categories SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;
```

### `patterns`
```sql
CREATE TABLE patterns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    name TEXT NOT NULL,
    description TEXT,
    category_id INTEGER REFERENCES pattern_categories(id) ON DELETE SET NULL,
    default_implementation_id INTEGER, -- FK added after implementations table exists
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

CREATE TRIGGER patterns_updated_at
    AFTER UPDATE ON patterns
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE patterns SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;
```

### `implementations`
```sql
CREATE TABLE implementations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    pattern_id INTEGER NOT NULL,
    name TEXT,                          -- Optional: "v2", "minimal", "club mode"
    graph_json TEXT NOT NULL DEFAULT '{"nodes":[],"edges":[]}',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (pattern_id) REFERENCES patterns(id) ON DELETE CASCADE
);

CREATE INDEX idx_implementations_pattern ON implementations(pattern_id);

CREATE TRIGGER implementations_updated_at
    AFTER UPDATE ON implementations
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE implementations SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;

-- Add FK constraint to patterns after implementations exists
-- (handled in migration order or via ALTER TABLE)
```

### `venue_implementation_overrides`
```sql
CREATE TABLE venue_implementation_overrides (
    venue_id INTEGER NOT NULL,
    pattern_id INTEGER NOT NULL,
    implementation_id INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    PRIMARY KEY (venue_id, pattern_id),
    FOREIGN KEY (venue_id) REFERENCES venues(id) ON DELETE CASCADE,
    FOREIGN KEY (pattern_id) REFERENCES patterns(id) ON DELETE CASCADE,
    FOREIGN KEY (implementation_id) REFERENCES implementations(id) ON DELETE CASCADE
);

CREATE TRIGGER venue_implementation_overrides_updated_at
    AFTER UPDATE ON venue_implementation_overrides
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE venue_implementation_overrides SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE venue_id = OLD.venue_id AND pattern_id = OLD.pattern_id;
    END;
```

### `tracks`
```sql
CREATE TABLE tracks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    track_hash TEXT NOT NULL UNIQUE,    -- SHA256 of audio content
    title TEXT,
    artist TEXT,
    album TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    duration_seconds REAL,
    file_path TEXT NOT NULL,            -- Local path to audio file
    storage_path TEXT,                  -- Cloud storage path (set after upload)
    album_art_path TEXT,
    album_art_mime TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT
);

CREATE TRIGGER tracks_updated_at
    AFTER UPDATE ON tracks
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE tracks SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;
```

### `track_beats`
```sql
CREATE TABLE track_beats (
    track_id INTEGER PRIMARY KEY,
    beats_json TEXT NOT NULL,           -- Array of beat times
    downbeats_json TEXT NOT NULL,       -- Array of downbeat times
    bpm REAL,
    downbeat_offset REAL,
    beats_per_bar INTEGER,
    logits_path TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_beats_updated_at
    AFTER UPDATE ON track_beats
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_beats SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE track_id = OLD.track_id;
    END;
```

### `track_roots`
```sql
CREATE TABLE track_roots (
    track_id INTEGER PRIMARY KEY,
    sections_json TEXT NOT NULL,
    logits_path TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_roots_updated_at
    AFTER UPDATE ON track_roots
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_roots SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE track_id = OLD.track_id;
    END;
```

### `track_waveforms`
```sql
CREATE TABLE track_waveforms (
    track_id INTEGER PRIMARY KEY,
    -- Preview data (synced to cloud)
    preview_samples_json TEXT NOT NULL,
    preview_colors_json TEXT,
    preview_bands_json TEXT,
    -- Full data (local-only, regenerated from audio)
    full_samples_json TEXT,
    colors_json TEXT,
    bands_json TEXT,
    sample_rate INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_waveforms_updated_at
    AFTER UPDATE ON track_waveforms
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_waveforms SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE track_id = OLD.track_id;
    END;
```

### `track_stems`
```sql
CREATE TABLE track_stems (
    track_id INTEGER NOT NULL,
    stem_name TEXT NOT NULL,            -- "vocals", "drums", "bass", "other"
    file_path TEXT NOT NULL,            -- Local path
    storage_path TEXT,                  -- Cloud storage path (set after upload)
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    PRIMARY KEY (track_id, stem_name),
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE TRIGGER track_stems_updated_at
    AFTER UPDATE ON track_stems
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE track_stems SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE track_id = OLD.track_id AND stem_name = OLD.stem_name;
    END;
```

### `scores`
```sql
CREATE TABLE scores (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    track_id INTEGER NOT NULL,
    name TEXT,                          -- Optional: "main", "chill version", etc.
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
);

CREATE INDEX idx_scores_track ON scores(track_id);

CREATE TRIGGER scores_updated_at
    AFTER UPDATE ON scores
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE scores SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;
```

### `score_annotations`
```sql
CREATE TABLE score_annotations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_id TEXT UNIQUE,
    score_id INTEGER NOT NULL,
    pattern_id INTEGER NOT NULL,
    start_time REAL NOT NULL,
    end_time REAL NOT NULL,
    z_index INTEGER NOT NULL DEFAULT 0,
    blend_mode TEXT NOT NULL DEFAULT 'replace',
    args_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 1,
    synced_at TEXT,
    FOREIGN KEY (score_id) REFERENCES scores(id) ON DELETE CASCADE,
    FOREIGN KEY (pattern_id) REFERENCES patterns(id) ON DELETE CASCADE
);

CREATE INDEX idx_score_annotations_score ON score_annotations(score_id);

CREATE TRIGGER score_annotations_updated_at
    AFTER UPDATE ON score_annotations
    FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN
        UPDATE score_annotations SET
            updated_at = CURRENT_TIMESTAMP,
            version = OLD.version + 1
        WHERE id = OLD.id;
    END;
```

### `settings`
```sql
CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

-- Default settings inserted on init:
-- artnet_enabled, artnet_interface, artnet_broadcast, artnet_unicast_ip
-- artnet_net, artnet_subnet, audio_output_enabled, max_dimmer
```

---

## Implementation TODO

### Phase 1: Local Database Restructure

#### 1.1 Clean up existing migration infrastructure
- [ ] Delete `/src-tauri/migrations/app/` directory
- [ ] Delete `/src-tauri/migrations/project/` directory
- [ ] Create fresh `/src-tauri/migrations/` directory with single schema file

#### 1.2 Create new unified schema
- [ ] Create `/src-tauri/migrations/001_initial_schema.sql` with all tables above
- [ ] Ensure foreign key order is correct (venues before fixtures, patterns before implementations, etc.)

#### 1.3 Update database initialization
- [ ] Remove `init_project_db()` function from `src-tauri/src/database.rs`
- [ ] Remove `ProjectDb` state from app state
- [ ] Update `init_app_db()` to use new migration path
- [ ] Remove project DB pool parameter from all commands

#### 1.4 Update Rust models
- [ ] Create `Venue` struct in `src-tauri/src/venues.rs` (new file)
- [ ] Update `Fixture` struct to include `venue_id`
- [ ] Create `Implementation` struct with new schema (own `id`, optional `name`)
- [ ] Update `Pattern` struct to include `default_implementation_id`
- [ ] Create `VenueImplementationOverride` struct
- [ ] Rename `TrackAnnotation` → `ScoreAnnotation`
- [ ] Create `Score` struct
- [ ] Add `remote_id: Option<String>`, `version: i32`, and `synced_at: Option<String>` to all syncable structs

#### 1.5 Update Tauri commands
- [ ] Create venue CRUD commands: `create_venue`, `get_venues`, `get_venue`, `update_venue`, `delete_venue`
- [ ] Update fixture commands to require `venue_id`
- [ ] Update implementation commands for new 1:many relationship
- [ ] Add `set_default_implementation` command
- [ ] Add `set_venue_implementation_override` command
- [ ] Rename annotation commands to score commands
- [ ] Add score CRUD: `create_score`, `get_scores`, `get_score`, `delete_score`
- [ ] Update annotation commands to work within scores

#### 1.6 Remove project file handling
- [ ] Delete `src-tauri/src/project_manager.rs` or repurpose for venue management
- [ ] Remove `create_project`, `open_project`, `close_project` commands
- [ ] Remove `recent_projects` table references
- [ ] Update frontend to remove project file dialogs

#### 1.7 Generate TypeScript bindings
- [ ] Run `cargo test` to regenerate TS types via `ts_rs`
- [ ] Update frontend imports for new/renamed types

### Phase 2: Frontend Updates

#### 2.1 Replace project concept with venues
- [ ] Update UI to show venue list instead of project files
- [ ] Add venue creation/editing UI
- [ ] Update fixture management to be venue-scoped

#### 2.2 Update pattern/implementation UI
- [ ] Show multiple implementations per pattern
- [ ] Add implementation selector in pattern editor
- [ ] Add "set as default" action for implementations
- [ ] Add per-venue override UI in venue settings

#### 2.3 Update track annotation UI
- [ ] Rename to "scores" throughout UI
- [ ] Add score selector/creator for tracks
- [ ] Support multiple scores per track

### Phase 3: Supabase Setup

#### 3.1 Create Supabase project
- [ ] Create new Supabase project
- [ ] Note project URL and anon key
- [ ] Configure auth callback URL for Tauri app

#### 3.2 Create Postgres schema
- [ ] Create tables mirroring local schema (use `remote_id` as primary key)
- [ ] Add appropriate indexes
- [ ] Configure RLS policies (permissive for MVP0)

#### 3.3 Create storage buckets
- [ ] Create `audio` bucket (public read for MVP0)
- [ ] Create `art` bucket (public read)
- [ ] Create `stems` bucket (public read)
- [ ] Configure upload size limits

#### 3.4 Add Supabase client to Tauri
- [ ] Add `supabase-rs` or `postgrest-rs` to Cargo.toml
- [ ] Create `src-tauri/src/cloud.rs` module
- [ ] Initialize Supabase client on app startup
- [ ] Store credentials securely (keyring or encrypted settings)

### Phase 4: Sync Implementation

#### 4.1 Sync infrastructure
- [ ] Create `SyncManager` struct to coordinate sync operations
- [ ] Implement connectivity detection
- [ ] Create sync queue for offline changes

#### 4.2 Push sync logic
- [ ] Add `synced_version INTEGER` column to track last-synced version (or store in separate sync metadata table)
- [ ] Query for records where `synced_at IS NULL OR version > synced_version`
- [ ] Batch upserts to Supabase Postgres
- [ ] Update `synced_at` and `synced_version` after successful push
- [ ] Handle conflicts with last-write-wins (compare `version` numbers)

#### 4.3 File upload
- [ ] Implement .wav → .mp3 conversion for tracks (use `symphonia` + `mp3lame` or shell out to ffmpeg)
- [ ] Implement stem compression before upload
- [ ] Upload files to appropriate Storage buckets
- [ ] Update `storage_path` column after successful upload

#### 4.4 Background sync
- [ ] Trigger sync on app launch
- [ ] Implement periodic sync (every 5 minutes when online)
- [ ] Add manual sync trigger command

#### 4.5 Partial sync for track_waveforms
- [ ] Only sync `preview_*` columns to cloud
- [ ] Regenerate `full_*` columns locally when needed

---

## File Changes Summary

### New Files
- `/src-tauri/migrations/001_initial_schema.sql`
- `/src-tauri/src/venues.rs`
- `/src-tauri/src/scores.rs`
- `/src-tauri/src/cloud.rs`
- `/src-tauri/src/sync.rs`

### Modified Files
- `/src-tauri/src/database.rs` — Remove project DB, update init
- `/src-tauri/src/lib.rs` — Update module exports and command registration
- `/src-tauri/src/fixtures/mod.rs` — Add venue_id handling
- `/src-tauri/src/fixtures/models.rs` — Update Fixture struct
- `/src-tauri/src/patterns.rs` — Add default_implementation_id
- `/src-tauri/src/implementations.rs` — New 1:many relationship (or create new file)
- `/src-tauri/src/annotations.rs` — Rename to scores, update logic
- `/src-tauri/src/tracks.rs` — Add storage_path handling
- `/src-tauri/Cargo.toml` — Add supabase/mp3 encoding deps

### Deleted Files
- `/src-tauri/migrations/app/*`
- `/src-tauri/migrations/project/*`
- `/src-tauri/src/project_manager.rs`

### Frontend (to be detailed separately)
- Remove project file handling
- Add venue management UI
- Add score management UI
- Update implementation selection UI
