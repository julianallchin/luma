# Data layer: deletes, ids and history

The server is the permanent record. A device is a cache of live rows.

This is the contract `supabase/migrations/20261112120000_data_layer.sql`
puts in place and the client keeps. It is data format 2: one cutover, no
compatibility with format 1 on either side. [sync.md](sync.md) describes the
transport underneath.

## Two kinds of tables

| Kind | Tables | Rule |
|---|---|---|
| Source | `venues`, `venue_members`, `fixtures`, `fixture_groups`, `fixture_group_members`, `venue_nodes`, `venue_node_params`, `venue_constraints`, `folders`, `folder_tracks`, `laser_zones`, `laser_limits`, `perform_patterns`, `perform_layouts`, `perform_controllers`, `control_bindings`, `tracks`, `track_beats`, `track_beat_validations`, `scores`, `clips`, `agent_threads`, `agent_thread_messages`, `agent_thread_transcript_heads` | The contract below |
| Derived | `track_stems`, `track_roots`, `track_drum_onsets`, `track_bar_classifications`, `track_genres`, and the local-only waveforms and embeddings | A cache: no flag, no history, deleted for real, rebuilt from audio |
| Scratch | `drafts` | As before: private, hard-deleted |

## The contract for a source table

- **`id` is random and opaque**: a UUID the writer generates. It names no
  parent and no handle, and is never used twice. That includes link and slot
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
was written against. The server refuses an edit to a grid row.

## History

One append-only table, `public.history`, and one trigger on every source
table except the three conversation tables, whose messages are their own
record.

`seq, table_name, row_id, op, before, after, actor_type, actor, thread_id,
message_id, change_group_id, scope_id, at, schema_version, uid`

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

The client sends these as request headers: `x-luma-actor`, `x-luma-thread`,
`x-luma-message`, `x-luma-group`, with `x-luma-format`.

## Migrations

- Server-side, over every row, flagged ones included. Outside a request
  nothing is refused.
- Each sets the actor to `migration:<file>` and bumps `public.data_format()`
  with `PROJECTION_VERSION`.
- Each ships a function that reads a row of the previous format as the new
  one (`public.row_from_format_1`), so old history entries stay readable.
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
