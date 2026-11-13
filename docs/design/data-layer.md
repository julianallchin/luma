# Data layer: deletes, ids and history

The server is the permanent record. A device is a cache of live rows.

This is the contract `supabase/migrations/20261112120000_data_layer.sql`
puts in place and the client keeps. It is data format 2: one cutover, no
compatibility with format 1 on either side. [sync.md](sync.md) describes the
transport underneath.

## Two kinds of tables

| Kind | Tables | Rule |
|---|---|---|
| Source | `feedback`, `agent_thread_usage`, `venues`, `venue_members`, `fixtures`, `fixture_groups`, `fixture_group_members`, `venue_nodes`, `venue_node_params`, `venue_constraints`, `folders`, `folder_tracks`, `laser_limits`, `perform_patterns`, `perform_layouts`, `perform_controllers`, `control_bindings`, `tracks`, `track_beats`, `track_beat_validations`, `scores`, `clips`, `agent_threads`, `agent_thread_messages`, `agent_thread_transcript_heads` | The contract below |
| Derived | `track_stems`, `track_roots`, `track_drum_onsets`, `track_bar_classifications`, `track_genres`, and the local-only waveforms and embeddings | A cache: no flag, no history, deleted for real, rebuilt from audio |
| Scratch | `drafts` | As before: private, hard-deleted |

## The contract for a source table

- **`id` is random and opaque**: a UUID the writer generates. It names no
  parent and no handle, and is never reused within a table. That includes link and slot
  rows (`folder_tracks`, `venue_node_params`, `venue_constraints`,
  `venue_members`, `laser_limits`, `perform_layouts`,
  `track_beat_validations`, `agent_thread_transcript_heads`), a venue's root
  node, and clips. A row that
  is "the one for this parent and key" is found by those columns, which are
  unique among live rows, not by building its id.
- **`uid` is the owner.** There is no `created_by`: the log's first entry for
  a row says who made it.
- **`deleted_at`, `deleted_by` exist on the server only.** A device never
  holds a flagged row, so local tables have neither column and local reads
  need no filter.
- **Agents never choose ids.** A clip an agent wants to refer to later gets a
  `label`, unique within its score among live clips.

## Delete, and undo

- The app deletes locally, exactly as before, cascades included.
- The upload connector sends a queued delete of a source row as
  `PATCH {deleted_at: <now>}`. A derived or scratch row is still an HTTP
  `DELETE`. The app's role has no `DELETE` on a source table.
- The server stamps `deleted_by`, flags the row's source children with the
  same stamp and deletes its derived and scratch children, in the same
  transaction. Parent and children leave every device in one checkpoint.
- The sync rules carry `deleted_at IS NULL`, so a flagged row is removed from
  devices that have it.
- **Undo is re-inserting the row under its id.** Every insert the connector
  uploads carries `deleted_at: null`, and an upsert onto a flagged row clears
  the flag. That is the only revive rule. Children come back the same way,
  one insert each; derived rows are recomputed.
- An edit that reaches the server after the row was flagged changes no row.
  The connector records that as "not saved", as it does any refused write.
- A second delete of a flagged row succeeds and keeps the first stamp.
- `deleted_by` is the account the request came from, whatever the request
  says.
- A membership is the one row with a narrower revive: only the venue's owner
  brings it back, and only one they removed. A member who left rejoins with
  the venue's code.
- A live row written under a flagged parent is refused as a foreign-key
  violation, so no device is sent a child whose parent it does not hold. A
  parent brought back uploads before its children.
- Where deleting was held more tightly than editing, the flag is held as
  tightly. Members edit a venue's row, and only its owner flags or revives
  it. A membership is flagged by its member or the venue's owner, revived
  only by the owner, and changed in no other way.

## Reads on the server

- Each source table has a `<table>_live` view. Helper functions that policies
  call (`can_access_venue`, `can_access_score`, `track_is_shared`, …) read
  parents through those views.
- A table's own policy never tests its own row's flag: flagging and reviving
  are writes to that row. So a flagged row stays writable by whoever could
  write it, and its children do not.

## Versioned inputs

A beat grid is written once. Reanalysis inserts a new `track_beats` row; the
track's current grid is its newest. `scores.grid_id` names the grid a score
was written against. The server refuses an edit to a grid row. A score made
before its song has a grid is given the first one when it is found. A beat
review names the grid it is about (`track_beat_validations.grid_id`), so a
review of one grid is not a verdict on another.

## History

One append-only table, `public.history`, and one trigger on every source
table except the three conversation tables, whose messages are their own
record.

`seq, table_name, row_id, op, before, after, actor_type, actor, thread_id,
message_id, change_group_id, scope_id, at, schema_version, uid, client_build,
venue_seq`

- `actor_type` is `person`, `agent`, `migration`, `app` or `admin`. `actor`
  is `user:<uid>`, a model id or MCP client label, `migration:<file>`,
  `app:<what>` (analysis, import, media sync) or the database role.
- `thread_id` and `message_id` are the conversation and the message that
  started the turn an agent's write belongs to.
- `change_group_id` groups changes made together: one agent turn, or one
  user action. Without one, the server transaction is the group.
- `scope_id` is the score, venue or track the row belongs to. Entries in one
  scope are in commit order, so a version of a score or venue is a `seq`.
- A flag change is an `update` with full `before` and `after`.
- A subagent's writes carry its own thread and message, and the change group
  of the turn it works for: the message a person sent at the top of its line
  of parents. One request reads as one group however far down the work went.
- `client_build` is the app version that sent the write, with its commit for
  a release or a nightly. A prompt, a tool list and the analysis versions
  all ship in a build, so this stands for them.
- `venue_seq`, on an entry for a score or a clip, is the newest entry of the
  score's venue when it was written. A clip names groups by id, and what
  those ids held is the venue at that `seq`. It is the server's venue: an
  edit made offline may have been made against an older one.

The client sends these as request headers: `x-luma-actor`, `x-luma-thread`,
`x-luma-message`, `x-luma-group`, `x-luma-build`, with `x-luma-format`.

## What people say

- `feedback` is a source table: a person's verdict (`rating`, 1 or -1) on an
  agent's turn or on one clip. A turn is named by the message that started
  it, the same `message_id` history records against what the turn wrote. A
  verdict is its author's alone, there is one per person and target among
  live rows, and it is kept when its clip is deleted. Its history is in the
  score's scope.
- `agent_thread_usage` is a source table: what a conversation cost so far
  (tokens by kind, the price where the writer was told one, time, subagents
  started), one live row a thread, written after each step of a turn as
  `app:usage`. Its history in the thread's scope is the spend step by step,
  each entry naming the model that ran. `thread_id` is not a foreign key:
  a hosted session's thread can be deleted before its price is known.
- `scores.copied_from_score_id` names the score a duplicate was made from,
  so a copy's clips are not read as work done twice.
- `profiles.roles`, `profiles.role_other` and `profiles.experience` are what
  an account said about itself on the "About you" page, which follows the
  terms: every role that fits, its own word for "other", and how often it
  has programmed lights. They are reached through `profile_background` and
  `set_profile_background`, never synced, and a change is a history entry
  in the profile's own scope. A skip is recorded as asked, with no answers.

## Migrations

- Server-side, over every row, flagged ones included. Outside a request
  nothing is refused.
- Each sets the actor to `migration:<file>` and bumps `public.data_format()`
  with `PROJECTION_VERSION`. The cutover itself is the exception: it rebuilds
  the log, so it writes no entries, and `public.legacy_ids` is its record.
- Each ships a function that reads a row of the previous format as the new
  one (`public.row_from_format_1`, `public.row_from_format_2`), so old
  history entries stay readable.
- Format 3 (`20261112150000_laser_projectors.sql`) is the laser projector
  as a built-in fixture with no channels, one window a venue, and laser
  nodes without a zone ([lasers.md](lasers.md)).
  `public.legacy_ids` maps the ids format 1 built from other columns to the
  ones that replaced them.

## Cutover on a device

A build at a new `PROJECTION_VERSION` clears its synced tables and downloads
again. Edits still waiting to upload were written in the old format and
cannot be sent: they are moved to the "not saved" list first, then the copy
is cleared. Local-only analysis caches and files are kept.

## Not here yet

Named versions and rollback (a bookmark on a `seq`, and forward changes that
restore an earlier state), erase (an admin operation that removes a row, its
children, its history and its storage objects), and training exports read
from snapshots. They need only the log.
