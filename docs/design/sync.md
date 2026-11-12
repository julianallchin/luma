# Sync

Luma syncs rows. PowerSync moves rows between the local SQLite database and
Supabase Postgres. There is no write server, no revision blobs, no projection
layer. Every domain writes its own tables in ordinary SQLite transactions.

This file is the transport. What a row's id is, what a delete means and what
the server remembers are in [data-layer.md](data-layer.md).

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
- A source table's `id` is a random UUID its writer generates, stored as an
  ordinary column ([data-layer.md](data-layer.md)). The five derived tables
  keep their natural key as the local primary key and carry `id` as a
  `GENERATED ALWAYS … VIRTUAL` column with a unique index: `track_stems`
  (`track_id || ':' || stem_name`), and `track_roots`, `track_drum_onsets`,
  `track_bar_classifications` and `track_genres` (`= track_id`). Generation is
  local only — in Postgres `id` is a plain `text primary key` the client
  supplies, and the triggers spell the concatenation themselves because a
  generated column is not reliably readable from a trigger.
- A row that is "the one for this parent and key" (a node's parameter, a
  thread's transcript head) is unique on those columns locally, and a
  download that finds the slot held under another id takes it over
  (`SyncedTable::slot`).
- Postgres row-level security decides who may read and write a row. Owner or
  venue member for venue content, the venue row included; owner for private
  content and for the venue's door (its owner, its share code, deleting it,
  removing members). A policy asks whether a row's parents are live, never
  whether the row itself is. `public.history` records who made each change.
  The app validates before it writes. The server does not re-run domain rules.
- Concurrent edits to one row merge per column, last write wins. PowerSync
  uploads only the columns a local write changed (`PATCH`).
- A device deletes for real, cascades included, and holds no flagged rows:
  local tables have no `deleted_at`. The server keeps a deleted source row
  and flags it; the sync rules select `deleted_at IS NULL`, so the row and
  its children leave every other device in one checkpoint. A derived or
  scratch row is deleted on both sides.
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
| `scores` | id, uid, track_id, venue_id, grid_id, name, authored_at, created_at, updated_at |
| `clips` | id, uid, score_id, label, name, graph_json, start, duration, seed (TEXT, decimal u64), selection_json, z_index, blend_mode, created_at, updated_at; `graph` and `inputs_json` are retired form columns, written empty and never read; `selection_seed` (TEXT, nullable) is retired too and never read |

A clip's `id` is a random UUID like any source row's, and says nothing about
its score. `label` is the handle an agent gave a clip it wants to refer to
later. It is unique within its score, so two scores may each have a `flash`,
and a clip nobody named has none. Each clip holds its own graph
(`graph_json`).

`scores.grid_id` names the `track_beats` row the score was written against.
A track has one `track_beats` row per analysis, never edited, and its current
grid is the newest.

`luma_patterns::Score` stays the in-memory type. Loading a score reads its
`scores` row and its clips into a `Score`. Saving a score diffs the candidate against the
rows and writes only the rows that changed. Editors and agents keep sending a
whole `Score`; the row diff happens once in the backend. There is no revision
token. A stale candidate simply overwrites the rows it touches.

## History

History is the server's and does not sync. One trigger, `record_history`, on
every source table except the three conversation tables appends to
`public.history` on each insert, update and delete. A delete arrives as an
update that sets the flag. An update whose only difference is `updated_at`
is skipped. The columns and what they mean are in
[data-layer.md](data-layer.md); `uid` is who made the change, and only they
can read their entries.

The client's part is to say who is writing. A write by an agent, or by the
app itself (analysis, import, media sync), runs in a transaction attributed
with `sync::triggers::attribute`, which takes an `Attribution`: the actor,
the thread, the message and the change group. The upload triggers put it in
each `powersync_crud` entry's metadata as one JSON object, and the connector
sends its parts as the `x-luma-actor`, `x-luma-thread`, `x-luma-message` and
`x-luma-group` headers. An unattributed write has no metadata and names no
actor, and the server records it as the signed-in person's. Upserts
attributed differently are never batched into one request.

- The app's own writes are `app:analysis`, `app:import` and `app:media-sync`.
- An agent turn's change group is the id of the message that started it. A
  score an MCP session mints names the client and the session message but no
  thread, which does not exist yet.
- Every request carries a group: the entry's own, or one the connector makes
  per local transaction and reuses when that transaction is retried.

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
venue_nodes, venue_node_params, venue_constraints, folders,
folder_tracks, laser_zones, laser_limits, perform_patterns, perform_layouts,
perform_controllers, control_bindings, tracks,
track_beats, track_roots, track_stems, track_drum_onsets,
track_bar_classifications, track_genres, track_beat_validations, scores,
clips, agent_threads, agent_thread_messages,
agent_thread_transcript_heads, drafts.

Each has a `Kind` in `backend/src/sync/schema.rs`: source, derived
(`track_stems`, `track_roots`, `track_drum_onsets`,
`track_bar_classifications`, `track_genres`) or scratch (`drafts`).

Local only: settings, control_bindings_unsynced (the bindings a machine kept
before they synced, until its venue's inputs move them), dmx_routes, preprocessing_failures,
track_waveforms, track_mert, agent_thread_runs,
agent_thread_usage, agent_thread_score_marks, sync_rejections, auth_write_admission, the
auth session, and the columns `tracks.file_path`, `tracks.album_art_path`,
`track_roots.logits_path`, `track_stems.file_path`.

`venues.environment` and `venues.haze` are **not** on that list: what kind of
room a venue is and what atmosphere it has are venue truth, so they follow the
venue between devices.

The local copy is a projection of the server and is never converted in place.
A local migration is DDL only; a format change is a server migration. When a
client can no longer read what an older one downloaded, bump
`PROJECTION_VERSION` in `backend/src/database/local/database.rs`. At startup a
library at another version clears its synced tables without queueing anything
for upload, and PowerSync downloads them again. Edits still waiting to upload
were written in the old format and cannot be sent, so they are moved to the
"not saved" list before the clear. Local-only tables and files are kept.

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

A migration that changes what synced rows mean is a new data format. In one
change: the Supabase migration replaces `public.data_format()` with the next
number, converts every row server-side, flagged ones included, and ships the
function that reads a row of the previous format as the new one
(`public.row_from_format_1`); the commit bumps `PROJECTION_VERSION`; and a
new synced table gets the `_client_format` trigger
(`supabase/migrations/20261111120000_client_format.sql`). There is no
compatibility between formats on either side.
[data-layer.md](data-layer.md) has the rules a migration keeps.

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
  resolution=merge-duplicates` for PUT, `PATCH` by id for an update). A
  queued delete of a source row is a `PATCH` that sets `deleted_at`, since
  the app's role has no `DELETE` there; a derived or scratch row is a
  `DELETE` by id. Every upsert carries `deleted_at: null`, so inserting a
  flagged id again brings the row back.
  A row-level-security policy refuses an upsert with a 403 but filters an
  update or delete to no row, which PostgREST answers with a 204. So `PATCH`
  and `DELETE` send `Prefer: count=exact`, and zero rows changed is a refusal
  (a delete of a row the user can no longer see is already done). An edit
  that arrives after its row was flagged changes no row either. Every
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
  adds `venue_id` to `venue_node_params`, `venue_constraints` and
  `fixture_group_members` so the sync rules can filter on it in one hop.
  `supabase/migrations/20261112120000_data_layer.sql` makes the server the
  permanent record: the flags, the random ids, the `_live` views and the
  history log. `deploy/README.md` lists every migration and its order.
- `deploy/powersync/sync-config.yaml`: the PowerSync Cloud sync rules. A `with:` clause is
  a parameter query and may return at most a thousand rows, so every one of
  them counts venues, scores or shared tracks — never their children. Every
  query on a source table selects `deleted_at IS NULL`.
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
