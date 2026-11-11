# Sync

Luma syncs rows. PowerSync moves rows between the local SQLite database and
Supabase Postgres. There is no write server, no revision blobs, no projection
layer. Every domain writes its own tables in ordinary SQLite transactions.

## Rules

- The synced tables are the app tables. PowerSync raw tables map onto them
  directly. Downloads write the app tables. Uploads read the app tables.
- Every synced table has an `id`, a `uid` (the owner), `created_at` and
  `updated_at`. In Postgres `id` is `text primary key` and `uid` is
  `uuid not null`. Locally the shape is looser: `uid` is `NOT NULL` only on
  `clips`, `drafts` and `venue_members`
  (`scores.uid` is deliberately nullable), and
  `agent_thread_messages.updated_at` and
  `agent_thread_transcript_heads.created_at` are nullable with no default, so
  every writer has to set them. `backend/src/sync/schema.rs` is the list.
- Twelve tables keep their natural key as the local primary key and carry `id`
  as a `GENERATED ALWAYS … VIRTUAL` column with a unique index. Four
  concatenate two columns — `venue_node_params` (`node_id || ':' || key`),
  `venue_constraints` (`node_id || ':' || my_socket`), `track_stems`
  (`track_id || ':' || stem_name`), `folder_tracks`
  (`folder_id || ':' || track_id`). The other eight alias a single column:
  `venue_edges.id = child_id`, each `track_*` analysis table `= track_id`,
  `agent_thread_transcript_heads.id = thread_id`. Generation is local only — in
  Postgres `id` is a plain `text primary key` the client supplies, and the
  triggers spell the concatenation themselves because a generated column is not
  reliably readable from a trigger.
- Postgres row-level security decides who may read and write a row. Owner or
  venue member for venue content, the venue row included; owner for private
  content and for the venue's door (its owner, its share code, deleting it,
  removing members). `public.history` records who made each change. The app
  validates before it writes. The server does not re-run domain rules.
- Concurrent edits to one row merge per column, last write wins. PowerSync
  uploads only the columns a local write changed (`PATCH`).
- Deletes are hard deletes on both sides. PowerSync replicates them.
- Local-only tables never appear in the PowerSync schema or the sync rules.
- A synced table's foreign keys are `DEFERRABLE INITIALLY DEFERRED`. A
  download is one transaction holding a consistent snapshot of the server in
  whatever order the checkpoint delivers it, so a child can land before its
  parent; an immediate check would reject the whole checkpoint, forever.
  Deferring only fixes the order. A parent the sync rules never ship is
  refused at the commit and refused again on every retry, so a rule that
  reaches a child reaches its parent too: the tracks behind a shared score.
- Sign-in is required to read or write the library. Every synced row has an
  owner, and a signed-in account is the only principal: signed out is a state
  of the host, in which only signing in works. The commands on an account's
  own rows — listing and importing tracks, listing venues, agent threads,
  sharing and joining venues — ask `AppServices::require_session`; venue
  content goes through `VenueAccess` and a track's through
  `VisibleTrackAccess`, which take the signed-in principal from the admission
  row in their own transaction and refuse without one. `VenueAccess` then
  admits by the local view `auth_venue_access` (migration
  `20261109120000_one_venue_standing.sql`). A clip carries its score's `uid`,
  and `clips.uid` is NOT NULL, so a score nobody owns cannot hold one.

  A library from before the rule may still hold rows whose `uid` is NULL. They
  are left where they are and no command reaches them. The schema still has
  room for them: the local views `auth_venue_access` and `auth_visible_tracks`
  keep a branch for an admission with no `active_uid`, and
  `agent_thread_messages` keeps a `principal_key` column whose CHECK admits
  `signed-out`.

## Score model

A score is rows.

| table | columns |
|---|---|
| `scores` | id, uid, track_id, venue_id, name, created_at, updated_at |
| `clips` | id, uid, score_id, name, graph_json, start, duration, seed (TEXT, decimal u64), selection_json, z_index, blend_mode, created_at, updated_at; `graph` and `inputs_json` are retired form columns, written empty and never read; `selection_seed` (TEXT, nullable) is retired too and never read |

A clip key is unique inside its score, not across the library — two scores
may each have a `flash`. Sync addresses every row by one global `id`, so
`clips` carries an ordinary `id` column the writer sets to
`score_id || ':' || key`, and the key is read back off it
(`backend/src/database/local/scores/rows.rs`). A score id is a uuid and carries
no colon, so the split is unambiguous however the key is spelled.

`luma_patterns::Score` stays the in-memory type. Loading a score reads its
`scores` row and its clips into a `Score`. Saving a score diffs the candidate against the
rows and writes only the rows that changed. Editors and agents keep sending a
whole `Score`; the row diff happens once in the backend. There is no revision
token. A stale candidate simply overwrites the rows it touches.

The old row format (`track_scores` with `pattern_id`) is gone. The cutover
migration copied its rows into the local-only table `legacy_scores_backup`.

Migration `20260923100000_drop_cues_and_pattern_library.sql` (local and
Supabase) drops the pattern library and MIDI cues: `patterns`,
`implementations`, `cues`, `pattern_categories`, `venue_implementation_overrides`,
`scores_pending_cutover`, `legacy_scores_backup` and the `auth_visible_patterns`
view. It also deletes MIDI bindings whose action fired a cue or a blackout.
Each clip holds its own graph (`graph_json`).

## History

History is the server's and does not sync. A Postgres trigger on each authored
table (venues and their content, `scores`, `clips`) appends to `public.history` on every insert, update and delete:

`history(id, uid, table_name, row_id, op, before, after, actor, at)`

An update whose only difference is `updated_at` is skipped. `uid` is who made
the change, and only they can read it. `actor` is `user:<uid>` for a person, or
a model id or MCP client label for an agent. An agent's write runs in a
transaction attributed with `sync::triggers::attribute`; the upload triggers
put the actor in each `powersync_crud` entry's metadata, and the connector
sends it as the `x-luma-actor` header. Upserts from different actors are never
batched into one request. See
`supabase/migrations/20260924000000_server_history.sql`.

Session undo in the editors stays in memory. A score's "last edited" time is
`scores.authored_at`, which `save_score` sets when a save moves something.

## Drafts

A subagent works on a draft, never on the live score.

`drafts(id, uid, score_id, thread_id, base_json, state_json, created_at, updated_at)`

Create copies the live `Score` into `base_json` and `state_json`. The agent's
`track.score_apply` writes `state_json` when the thread owns a draft. Merge
diffs `state_json` against `base_json` per clip and writes
those changes onto the live rows, then deletes the draft. Discard deletes the
draft. Drafts sync to their owner only.

## Conversations

`agent_threads`, `agent_thread_messages` and `agent_thread_transcript_heads`
sync to their owner as rows. Append receipts, deletion receipts, turn
preparations, turn outcomes and operation outcomes are gone: a retried write
is an upsert.

## Synced tables

venues, venue_members, fixtures, fixture_groups, fixture_group_members,
venue_nodes, venue_edges, venue_node_params, venue_constraints, folders,
folder_tracks, laser_zones, laser_limits, perform_patterns, perform_layouts,
perform_controllers, control_bindings, tracks,
track_beats, track_roots, track_stems, track_drum_onsets,
track_bar_classifications, track_genres, track_beat_validations, scores,
clips, agent_threads, agent_thread_messages,
agent_thread_transcript_heads, drafts.

Local only: settings, control_bindings_unsynced (the bindings a machine kept
before they synced, until its venue's inputs move them), dmx_routes, preprocessing_failures,
track_waveforms, track_mert, agent_thread_runs,
agent_thread_usage, sync_rejections, auth_write_admission, the
auth session, and the columns `tracks.file_path`, `tracks.album_art_path`,
`track_roots.logits_path`, `track_stems.file_path`.

The local copy is a projection of the server and is never converted in place.
A local migration is DDL only; a format change is a server migration. When a
client can no longer read what an older one downloaded, bump
`PROJECTION_VERSION` in `backend/src/database/local/database.rs`. At startup a
library at another version clears its synced tables without queueing anything
for upload, and PowerSync downloads them again. A change still waiting to
upload postpones that to a later launch.

## Required updates

A client keeps running after a newer release ships, and a build from before a
format change would misread the new rows and write old ones over them. So the
data has a format number, and a build behind it is turned away.

- The server's number is `public.data_format()`. The client's is
  `PROJECTION_VERSION`: the two are one number, bumped together by the
  migration and the commit that change what synced rows mean.
- **Writes.** Every PostgREST request carries the client's number in
  `x-luma-format`. A statement trigger on every synced table refuses a write
  from a build behind the server with HTTP 426, whether it comes as a row or
  through a function. No header means a build from before the header, which
  reads format 1. The refusal is recorded like any other.
- **Reads.** Before it opens the sync stream, and once a minute while
  connected, the client asks `data_format()`. Behind it, the client
  disconnects and does not reconnect, so it applies nothing it would misread.
  A server that will not answer (offline, or not migrated) is not a reason to
  stay disconnected: the write check is the server's own.
- **The app.** `SyncStatus.update_required` goes up. The window shows "Update
  Luma to continue" in place of the app and fetches the update whatever the
  auto-update setting says. "Not Now" brings the app back for the run, on the
  local copy as it was; the status bar keeps saying `Update required`.

What it does not cover: a connected build keeps applying downloads for up to
a minute after the server moves, and an offline one cannot know until it
reconnects. The updated build re-downloads either way, because the number
that changed is `PROJECTION_VERSION`. A release older than 0.7.1 has no read
check at all; the server still refuses its writes.

To require an update, in one change: replace `public.data_format()` with the
next number in the Supabase migration that changes the rows, bump
`PROJECTION_VERSION`, and give a new synced table the `_client_format`
trigger (`supabase/migrations/20261111120000_client_format.sql`).

`venues.environment` and `venues.haze` are **not** on that list: what kind of
room a venue is and what atmosphere it has are venue truth, so they follow the
venue between devices. Only the three hardware-port columns above are local.

## Code layout

- `backend/src/sync/service.rs`: `Service`. Connects PowerSync when a session
  exists, disconnects on sign-out, exposes status, emits `replica-changed`
  when synced tables change. The host starts it from the connection pair
  `database::local::database::open_app_db_at` returns.
- `backend/src/sync/schema.rs`: the synced table list with columns. Generates
  the PowerSync `RawTable` put and delete statements.
- `backend/src/sync/triggers.rs`: generates the TEMP triggers for writer
  connections, into `powersync_crud`.
- `backend/src/sync/connector.rs`: `fetch_credentials` returns the PowerSync
  Cloud endpoint and the Supabase access token. `upload_data` posts each CRUD
  transaction to Supabase PostgREST (`POST` with `Prefer:
  resolution=merge-duplicates` for PUT, `PATCH` by id, `DELETE` by id).
  A row-level-security policy refuses an upsert with a 403 but filters an
  update or delete to no row, which PostgREST answers with a 204. So `PATCH`
  and `DELETE` send `Prefer: count=exact`, and zero rows changed is a refusal
  (a delete of a row the user can no longer see is already done). Every
  refusal is recorded in the local `sync_rejections` table and taken off the
  queue; the next download puts the server's row back. The status bar counts
  the undismissed ones as `N not saved`.
- `backend/src/sync/media.rs`, `files.rs`, `progress.rs`: media transfer, on
  its own clock, independent of record sync. Bytes go through Supabase
  Storage; only the resulting `storage_path` is a row. It names the bucket
  too (`track-audio/<uid>/<hash>/audio.ogg`), while a Storage object's name
  does not, so the read policy `can_read_track_media` compares
  `bucket || '/' || name` against it. Storage reports an object its policy
  hides as missing (a 400 with `NoSuchKey`).
- `backend/crates/sync`: SDK glue only (shared SQLite file between SQLx and the
  SDK pool, blocking actor tasks).
- `supabase/migrations/20260912000000_row_model.sql`: drops the old schema and
  creates every synced table, its RLS policies and the `powersync`
  publication. `supabase/migrations/20260916000000_stage_child_venue_id.sql`
  adds `venue_id` to `venue_edges`, `venue_node_params`, `venue_constraints`
  and `fixture_group_members` so the sync rules can filter on it in one hop.
  `supabase/migrations/20260923100000_drop_cues_and_pattern_library.sql`
  drops `patterns`, `implementations` and `cues` and their read functions.
  All three are required on the server.
- `deploy/powersync/sync-config.yaml`: the PowerSync Cloud sync rules. A `with:` clause is
  a parameter query and may return at most a thousand rows, so every one of
  them counts venues, scores or shared tracks — never their children.
- `deploy/test-stack/run.py`: disposable Postgres, PostgREST and PowerSync
  containers for the two-device tests.

## PowerSync Cloud

1. Create an instance. Region: same as the Supabase project.
2. Connect it to the Supabase Postgres with the replication role and the
   `powersync` publication.
3. Client Auth: enable "Use Supabase Auth".
4. Run `powersync deploy sync-config --directory=deploy/powersync` once;
   from then on every release deploys it (deploy/README.md).
5. Put the instance URL in `backend/src/config.rs` as `POWERSYNC_URL`.
