# Database

Luma keeps all library data in one SQLite database, `luma.db`. Venues,
fixtures, groups, tracks, scores and patterns are rows in this database. The
auth session is in a separate `state.db` in the same directory.

## Local database

File: `backend/src/database/local/database.rs`

- The database uses WAL journal mode and foreign keys.
- `open_app_db_at` runs the migrations and opens the pool that the app writes
  through.
- Every pooled connection gets TEMP triggers that write the PowerSync upload
  queue (`backend/src/sync/triggers.rs`).

## Migrations

- Local migrations are in `backend/migrations`. `sqlx::migrate!` runs them at
  startup, in filename order.
- Migrations are append-only. A changed file breaks the sqlx checksum. To change
  the schema, add a new file.
- Remote migrations are in `supabase/migrations`. A schema change to a synced
  table needs a file in both directories. Each release applies the pending
  ones to the server before it builds the app. A migration that removes
  something carries a `-- contract: after <version>` line and runs only in a
  release after the one whose clients stopped using it.
- A migration that changes what synced rows mean is a new data format. It
  converts every row on the server and raises `public.data_format()`, and
  the app's `PROJECTION_VERSION` rises with it. A build behind the server is
  told to update. The updated build clears its synced tables and downloads
  them again; no code converts a local row.

## Sync

Luma syncs rows. PowerSync moves rows between the local SQLite database and
Supabase Postgres. There is no write server.

- Every synced table has `id`, `uid` (the owner), `created_at` and
  `updated_at`.
- Postgres row-level security decides who can read and write a row.
- Two edits to one row merge per column. The last write wins. An upload sends
  only the changed columns.
- The server is the permanent record, and a device is a cache of live rows.
- Sign-in is required to write. Reading local data does not need a session.

### Source and derived tables

Every synced table is one of three kinds (`Kind` in
`backend/src/sync/schema.rs`):

| Kind | Tables | Rule |
|------|--------|------|
| Source | What people and agents make: venues and their content, tracks, beat grids and their validations, scores, clips, conversations | Kept by the server, with the rules below |
| Derived | `track_stems`, `track_roots`, `track_drum_onsets`, `track_bar_classifications`, `track_genres` | A cache. Deleted on both sides and rebuilt from the audio |
| Scratch | `drafts` | Private to its owner. Deleted on both sides |

A source row's `id` is a random UUID that the writer generates. It names no
parent, and no id is built from other columns. A row that is the only one
for its parent and key, such as a node's parameter, is found by those
columns.

### Deletes

- The app deletes a row from the local database, with its children.
- The upload sends the delete of a source row as an update that sets
  `deleted_at`. The server keeps the row, flags its source children and
  deletes its derived children in the same transaction.
- The sync rules select only rows where `deleted_at` is null. The row and its
  children leave every other device together.
- A device holds no flagged rows. Local tables have no `deleted_at` column.
- Inserting the row again under its id clears the flag. That is how an undo
  reaches the server.
- An edit that reaches the server after its row was flagged changes nothing.
  The app lists it as not saved.

### Synced tables

The list is `SYNCED_TABLES` in `backend/src/sync/schema.rs`:

| Area | Tables |
|------|--------|
| Venues | `venues`, `venue_members`, `venue_nodes`, `venue_node_params`, `venue_constraints` |
| Fixtures and groups | `fixtures`, `fixture_groups`, `fixture_group_members` |
| Tracks | `tracks`, `track_beats`, `track_roots`, `track_stems`, `track_drum_onsets`, `track_bar_classifications`, `track_genres`, `track_beat_validations` |
| Scores | `scores`, `clips`, `drafts`, `folders`, `folder_tracks` |
| Lasers | `laser_limits`; a projector's settings and the venue's window are `venue_node_params` |
| Perform | `perform_patterns`, `perform_layouts`, `perform_controllers`, `control_bindings` |
| Agents | `agent_threads`, `agent_thread_messages`, `agent_thread_transcript_heads` |

Local-only tables include `settings`, `dmx_routes`,
`preprocessing_failures`, `track_waveforms` and `track_mert`. Some
columns of synced tables are also local, such as `tracks.file_path`.

### Scores as rows

| Table | Columns |
|-------|---------|
| `scores` | id, uid, track_id, venue_id, grid_id, name, authored_at, created_at, updated_at |
| `clips` | id, uid, score_id, label, name, graph_json, start, duration, seed, selection_json, z_index, blend_mode, created_at, updated_at, and the unread `graph`, `inputs_json` and `selection_seed` |

A clip id is a random UUID. A clip that an agent named also has a `label`,
which is unique within its score. Loading a score reads the two tables into a
`luma_patterns::Score`. Saving a score compares it with the
rows and writes only the rows that changed (`backend/src/database/local/scores/rows.rs`).

A track's beat grid is a `track_beats` row that is written once. A new
analysis inserts a new row, and the newest row is the track's current grid.
`scores.grid_id` names the grid that a score was written against.

A clip's graph is JSON in `graph_json`. A score write checks each changed
clip with `Score::validate_changes`, which runs `clip_graph::check_clip` and
lowers the graph. The `graph` and `inputs_json` columns are left from the form
model (`supabase/migrations/20260929000000_clip_graphs.sql`): nothing reads
them, and a new row writes them empty. Migration
`20260923100000_drop_cues_and_pattern_library.sql` dropped the old `patterns`,
`implementations`, `cues` and `pattern_categories` tables. Migration
`20260923200000_drop_score_definitions.sql` dropped `score_definitions`.

## History

History is the server's. One Postgres trigger appends each change to a source
row, except conversation rows, to the `public.history` table
(`supabase/migrations/20261112120000_data_layer.sql`). Devices keep no change
log.

- An entry holds the row before and after the change.
- `actor_type` is `person`, `agent`, `migration`, `app` or `admin`. `actor`
  is `user:<uid>` for a person, a model id or MCP client label for an agent,
  and `app:<what>` for analysis, import and media sync.
- An agent's entry also names the conversation and the message that started
  the turn.
- `change_group_id` groups the changes of one agent turn or one user action.
- `scope_id` is the score, venue or track that the row belongs to. Entries in
  one scope are in commit order.
- The client sends the actor, thread, message and group as request headers
  with each upload.

## Drafts

A subagent edits a draft, not the live score. A `drafts` row has `base_json`
and `state_json`. A merge compares the two per clip, writes the
differences onto the live rows, then deletes the draft.

## Code layout

| File | Role |
|------|------|
| `backend/src/sync/service.rs` | Connects PowerSync when a session exists. Sends one `replica-changed` event for downloaded writes. |
| `backend/src/sync/schema.rs` | Synced tables and columns. Generates the PowerSync raw table statements. |
| `backend/src/sync/triggers.rs` | Generates the TEMP triggers. |
| `backend/src/sync/connector.rs` | Gets credentials and uploads changes to Supabase PostgREST. |
| `backend/src/sync/media.rs`, `files.rs` | Moves audio, stems and album art through Supabase Storage. |
| `backend/crates/sync` | PowerSync SDK glue. |
| `deploy/powersync/sync-config.yaml` | PowerSync sync rules. |

## Media

Audio files, stems and album art do not go through row sync. A separate loop
runs every 60 seconds while a session exists. It uploads files that have a
local file and no `storage_path`. It downloads files that have a
`storage_path` and no local file. An upload writes the bytes first and the
`storage_path` second.

The design documents are `docs/design/data-layer.md` and
`docs/design/sync.md` in the repository.
