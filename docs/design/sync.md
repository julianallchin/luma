# Sync

Luma syncs rows. PowerSync moves rows between the local SQLite database and
Supabase Postgres. There is no write server, no revision blobs, no projection
layer. Every domain writes its own tables in ordinary SQLite transactions.

## Rules

- The synced tables are the app tables. PowerSync raw tables map onto them
  directly. Downloads write the app tables. Uploads read the app tables.
- Every synced table has `id TEXT PRIMARY KEY`, `uid TEXT NOT NULL` (the owner),
  `created_at` and `updated_at`. Tables with a natural composite key get a
  generated `id` column (`a || ':' || b`).
- Postgres row-level security decides who may read and write a row. Owner or
  venue member for venue content, owner for private content, everyone for
  verified library patterns. The app validates before it writes. The server
  does not re-run domain rules.
- Concurrent edits to one row merge per column, last write wins. PowerSync
  uploads only the columns a local write changed (`PATCH`).
- Deletes are hard deletes on both sides. PowerSync replicates them.
- Local-only tables never appear in the PowerSync schema or the sync rules.
- A synced table's foreign keys are `DEFERRABLE INITIALLY DEFERRED`. A
  download is one transaction holding a consistent snapshot of the server in
  whatever order the checkpoint delivers it, so a child can land before its
  parent; an immediate check would reject the whole checkpoint, forever.
- Sign-in is required to write. Every synced row has an owner, and there is no
  signed-out owner: `AppServices::require_session` refuses the command and the
  shell shows the sign-in screen. Reading a library already on this machine
  needs nothing.

## Score model

A score is rows.

| table | columns |
|---|---|
| `scores` | id, uid, track_id, venue_id, name, created_at, updated_at |
| `clips` | id, uid, score_id, graph, start, duration, seed (TEXT, decimal u64), selection_seed (TEXT, nullable), selection_json, z_index, blend_mode, inputs_json, created_at, updated_at |
| `score_definitions` | id, uid, score_id, definition_json, created_at, updated_at |

A clip key and a definition key are unique inside their score, not across the
library — two scores may each have a `flash`. Sync addresses every row by one
global `id`, so both tables store `score_id || ':' || key` and the key is read
back off it. A score id is a uuid and carries no colon, so the split is
unambiguous however the key is spelled.

`luma_patterns::Score` stays the in-memory type. Loading a score reads the
three tables into a `Score`. Saving a score diffs the candidate against the
rows and writes only the rows that changed. Editors and agents keep sending a
whole `Score`; the row diff happens once in the backend. There is no revision
token. A stale candidate simply overwrites the rows it touches.

The old row format (`track_scores` with `pattern_id`) is gone. Its rows were
exported to `~/.config/com.luma.luma/backups/legacy-scores-*.json` and copied
into the local-only table `legacy_scores_backup` by the cutover migration.

`patterns`, `implementations` and `cues` stay for live MIDI cues. They sync as
plain rows with `graph_json` as one column.

## History

The local-only trigger set on every writer connection appends one row to
`changes` for every insert, update and delete on a synced table:

`changes(id, uid, table_name, row_id, op, before_json, after_json, actor, at)`

`uid` is who made the change, not who owns the row: a venue member editing the
owner's clip writes a change of their own.

`changes` syncs to its owner. Session undo in the editors stays in memory.
Restore to a past point replays `changes` for a score, venue or pattern. No UI
exposes restore yet; the data is there.

Download application runs on the SDK connection, which has no TEMP triggers, so
downloaded rows do not produce `changes` or upload entries.

## Drafts

A subagent works on a draft, never on the live score.

`drafts(id, uid, score_id, thread_id, base_json, state_json, created_at, updated_at)`

Create copies the live `Score` into `base_json` and `state_json`. The agent's
`track.score_apply` writes `state_json` when the thread owns a draft. Merge
diffs `state_json` against `base_json` per clip and per definition and writes
those changes onto the live rows, then deletes the draft. Discard deletes the
draft. Drafts sync to their owner only.

## Conversations

`agent_threads`, `agent_thread_messages` and `agent_thread_transcript_heads`
sync to their owner as rows. Append receipts, deletion receipts, turn
preparations, turn outcomes and operation outcomes are gone: a retried write
is an upsert.

## Synced tables

venues, venue_members, fixtures, fixture_groups, fixture_group_members,
venue_nodes, venue_edges, venue_node_params, venue_constraints, tracks,
track_beats, track_roots, track_stems, track_drum_onsets,
track_bar_classifications, track_genres, track_beat_validations, scores,
clips, score_definitions, patterns, implementations, cues, midi_modifiers,
midi_bindings, agent_threads, agent_thread_messages,
agent_thread_transcript_heads, drafts, changes.

Local only: settings, universe_outputs, preprocessing_failures,
preprocessing_runs, track_waveforms, track_mert, fixture_group_overrides,
pattern_categories, venue_implementation_overrides, agent_thread_runs,
agent_thread_usage, legacy_scores_backup, the auth session, and the columns
`venues.controller_port`, `venues.mixer_port`, `venues.mixer_mapping_json`,
`tracks.file_path`, `tracks.album_art_path`, `track_roots.logits_path`,
`track_stems.file_path`.

## Code layout

- `backend/src/sync/service.rs`: `Service`. Connects PowerSync when a session
  exists, disconnects on sign-out, exposes status, emits `replica-changed`
  when synced tables change. The host starts it from the connection pair
  `database::local::database::open_app_db_at` returns.
- `backend/src/sync/schema.rs`: the synced table list with columns. Generates
  the PowerSync `RawTable` put and delete statements.
- `backend/src/sync/triggers.rs`: generates the TEMP triggers for writer
  connections: one set into `powersync_crud`, one set into `changes`.
- `backend/src/sync/connector.rs`: `fetch_credentials` returns the PowerSync
  Cloud endpoint and the Supabase access token. `upload_data` posts each CRUD
  transaction to Supabase PostgREST (`POST` with `Prefer:
  resolution=merge-duplicates` for PUT, `PATCH` by id, `DELETE` by id).
- `backend/src/sync/media.rs`, `files.rs`, `progress.rs`: media transfer, on
  its own clock, independent of record sync. Bytes go through Supabase
  Storage; only the resulting `storage_path` is a row.
- `backend/crates/sync`: SDK glue only (shared SQLite file between SQLx and the
  SDK pool, blocking actor tasks).
- `supabase/migrations/20260912000000_row_model.sql`: drops the old schema and
  creates every synced table, its RLS policies and the `powersync`
  publication.
- `deploy/sync-rules.yaml`: the PowerSync Cloud sync rules. A `with:` clause is
  a parameter query and may return at most a thousand rows, so every one of
  them counts venues, scores or shared tracks — never their children. That is
  why the stage and group child rows carry a `venue_id` of their own.
- `experiments/powersync/run.py`: disposable Postgres, PostgREST and PowerSync
  containers for the two-device tests.

## PowerSync Cloud

1. Create an instance. Region: same as the Supabase project.
2. Connect it to the Supabase Postgres with the replication role and the
   `powersync` publication.
3. Client Auth: enable "Use Supabase Auth".
4. Paste `deploy/sync-rules.yaml`.
5. Put the instance URL in `backend/src/config.rs` as `POWERSYNC_URL`.

## Measurement

Measure runtime and test source separately against the dev merge base
`36e87427`. Do not count migrations as runtime.
