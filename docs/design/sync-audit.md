# Sync engine audit

Branch `agent-code-execution`, 2026-08-27. Read-only audit of `src-tauri/src/sync/**`, `database/remote/**`, `services/authored_documents/remote_sync.rs`, `services/authored_sync_merge.rs`, the Supabase migrations under `supabase/migrations/`, and the live Postgres schema (catalog-table DDL is not in the repo; those facts were introspected from the live project and are marked *live*).

Each finding is marked **CONFIRMED** (code path traced end to end) or **PLAUSIBLE** (mechanism is present, triggering conditions not fully verified).

Two framing corrections before anything else:

1. `project_sync_engine.md` and the task brief describe the pull cursor as "dirty bit + keyset `(updated_at, id)`". That is stale. The cursor is now a server-assigned `sync_seq` (`registry.rs:19`, `state.rs`), stored as `seq:<n>` in the legacy `sync_state.last_pulled_at` column. The historic "cursor advances to `now`, mid-batch rows skipped forever" bug is **CONFIRMED fixed**, not narrowed — see §2.1. Client-clock skew no longer touches the cursor at all.
2. `registry.rs` and `pull.rs` are mid-edit by another agent (working tree adds `authored_revisions.actor`, `agent_threads.parent_thread_id/parent_call_id`, `authored_turn_preparations.workspace_id`). Everything below was read from the working tree, so those columns are included.

---

## 1. Map

### 1.1 Topology

```
                 Tauri app only (lib.rs:341)              gpui app / luma-mcp
                 ┌──────────────────────────┐             ┌──────────────────┐
                 │ push::run_sync_loop       │             │ no loop at all   │
                 │  every 10s: enqueue_dirty │             │ writes land in   │
                 │            + flush_pending│             │ pending_ops and  │
                 │  every 60s: run_pull_cycle│             │ wait for Tauri   │
                 └────────────┬─────────────┘             └──────────────────┘
                              │ sync_lock (tokio Mutex)
        ┌─────────────────────┼───────────────────────────┐
        ▼                     ▼                           ▼
  pull.rs                 push.rs / pending.rs         files.rs
  discover_venues         fetch_ready_ops (topo sort)  storage upload/download
  pull_all (topo order)   execute_op                   driven by storage_path
  pull_table (seq pages)    upsert / insert_immutable   NULL vs .stub file_path
  execute_upsert            delete (PATCH deleted_at)
  delete_local              3 authored RPCs
                          mark_synced / remove_op
        │                     │
        ▼                     ▼
  traits::RemoteClient  ←── supabase_remote.rs ←── database/remote/common.rs (reqwest)
        │
        ▼
  Supabase: PostgREST tables + 3 SECURITY DEFINER RPCs + private.next_sync_seq()
```

`orchestrator::SyncEngine::sync_full` (dispatch `sync_full` command, `join_venue`) is the same pipeline run on demand: discovery → pull → files → push. `patterns.rs` handlers poke `push_notify` after writes.

### 1.2 Row sync (catalog + immutable traces)

- **Registry** (`registry.rs:259-853`): 32 `TableMeta` entries with `columns`, `local_only`, `parents`. Topological order via Kahn (`tables_in_topo_order`). Push policy per table: `DirtyUpsert` (16 catalog tables), `ExplicitImmutable` (10 append-only tables), `ExplicitUpsert` (`agent_threads`), `ServerAuthority` (heads, proposals, integrations, archives, transcript heads). Pull policy: `DirtyUpsert`, `Immutable`, `ProjectionUpsert`, `ThreadProjection`, `ServerEnriched`, `TerminalArchive`.
- **Dirty bit**: SQLite `*_updated_at` triggers (`migrations/20260802200000_explicit_sync_dirtiness.sql`) fire `WHEN OLD.version = NEW.version` and set `updated_at = now, version+1, synced_at = NULL`. `enqueue_dirty` (`orchestrator.rs:250-338`) scans `synced_at IS NULL` per table (limit 1000), snapshots the row as JSON, and upserts into `pending_ops` (dedup on `(principal_key, table_name, record_id, op_type)`).
- **Push**: `flush_pending_with_integrator` (`push.rs:36-123`) checks token uid == admitted uid, fetches up to 1000 ready ops ordered by `created_at`, re-sorts by topo position, truncates to 100, executes sequentially. Success → `mark_synced` (`synced_at = updated_at, version+1`) then `remove_op`. 401 aborts the batch; 409 and other errors → `record_failure` (exp backoff, dead-letter at 20); network error aborts the batch without touching attempts.
- **Deletes**: SQLite `sync_delete_*` triggers (`WHEN OLD.origin = 'local'`) enqueue `delete` ops; push PATCHes `deleted_at` (client clock). Pull hard-deletes locally on `deleted_at IS NOT NULL` via `delete_local`, gated by `ensure_remote_delete_is_safe`.
- **Pull** (`pull.rs:313-515`): per table, `sync_seq=gt.{cursor}&order=sync_seq.asc&limit=500`, pages until empty. Per row: locally-dirty → skip (cursor advances); tombstone → `delete_local`; `authored_document_heads` → `apply_server_head`; else `execute_upsert` (one `BEGIN IMMEDIATE` per row under `enter_remote_writes`). Any non-refusal error stops the table; cursor advanced only to `last_successful_cursor`. Children of a failed table are deferred in the same cycle.
- **Server cursor**: `private.next_sync_seq()` row-locks the single row of `private.luma_sync_clock` (`sync.sql:12-39`); `sync_seq_bump BEFORE INSERT OR UPDATE` on catalog tables, `immutable_assign_sync_seq BEFORE INSERT` on immutable tables. Because the lock is held to commit, `sync_seq` is commit-ordered.

### 1.3 Authored sync (revision DAG + head proposals)

- Revisions, files, parents, documents are `ExplicitImmutable` rows enqueued in the creating transaction (`enqueue_revision_closure`). The remote immutable trigger (`immutable_update_or_identical`) accepts an exact replay and raises 23505 on any different byte.
- Heads move only through RPCs (`authored_remote.rs`): `submit_authored_head_proposal` (document row `FOR UPDATE`, asserts revision closure exists, assigns `server_proposal_seq`), `integrate_authored_head_proposal` (earliest-pending check, CAS on `expected_head_revision_id`, per-resolution validation, `generation+1`), `archive_authored_document` (advisory lock on route, cancels pending proposals, projects `scores.deleted_at` / `patterns.deleted_at`).
- Integration is a durable *wake-up* op (`INTEGRATE_HEAD_PROPOSAL_OP`, payload = proposal id only). `AuthoredDocuments::integrate_pending_proposal` (`remote_sync.rs:831-931`) reads the live server head, computes a resolution in `prepare_proposal_integration` (`AlreadyAncestor` / `FastForward` / `WholeProposal` / `Structural` / `QuarantinedNoop`), uploads the structural merge closure inline, calls the RPC, and applies the receipt head locally. Non-terminal receipts (`Stale`, `NotEarliest`) retry forever with capped backoff (`record_integration_retry`, never dead-letters).
- Any online owner device integrates any pending proposal it has pulled (`enqueue_pending_head_integrations`, `remote_sync.rs:293-321`), so an offline author is never required.
- Server head projection on pull: `apply_server_head_observation` (`remote_sync.rs:97-288`) reads the local head, re-reads it inside `BEGIN IMMEDIATE`, errors on movement, then rewrites the live projection (`track_scores` / `implementations`) to the server revision and CASes the local head.

---

## 2. Correctness audit

### 2.1 Cursor semantics — CONFIRMED fixed

The old failure (cursor set to wall-clock `now`; rows committed with an earlier `updated_at` after the pull started were skipped) cannot recur:

- Cursor is the last *successfully processed* `sync_seq`, not a timestamp (`pull.rs:502-505`).
- `sync_seq` is allocated under a row lock held until commit (`sync.sql:20-39`), so no committed row can carry a `sync_seq` lower than one already visible. Long-running uploads are serialized at the clock, not raced.
- `advance_last_pulled_seq` is monotonic under concurrent replays (`state.rs:38-72`).
- New visibility (new owned venue, new membership) resets every cursor for that uid to 0 (`pull.rs:156-165`), covering the "joined after tracks were created" case.

**Residual, quantified:** client clocks are now irrelevant to *what* gets pulled. They still reach the data in three places: (a) `updated_at` on the 5 catalog tables with no server `BEFORE UPDATE` trigger (fixture_groups, midi_modifiers, fixture_group_members, cues, midi_bindings — *live*) and on the INSERT half of every upsert; (b) `deleted_at` in `push.rs:251-253`; (c) `synced_at = row.updated_at` on pull (`pull.rs:668-673`), which is only ever compared to NULL so skew is cosmetic there. Nothing orders or merges on those timestamps, so skew is a display/audit problem, not a correctness one. Effective cost: zero rows skipped; audit timestamps may be off by whatever the device clock is off by.

**New residual (the one that matters):** `sync_seq_bump` fires on every UPDATE, including an exact-replay upsert of unchanged bytes (*live*, `sync.sql:54-64`). A device re-pushing an already-synced row (e.g. after a `mark_synced` failure) bumps the row's `sync_seq`, forcing every other device to re-pull it. Harmless for correctness, but it means "rows pulled > 0" is not "something changed", and it will inflate the `library-changed` event rate.

### 2.2 Pull page transactionality — CONFIRMED not atomic, by design

Each row is its own `BEGIN IMMEDIATE` (`pull.rs:696-724`). A failure mid-page leaves earlier rows applied and the cursor at the last success; the next cycle resumes at the failing row. That is correct given seq ordering and idempotent upserts. The cost is per-row transaction overhead (500 rows = 500 write transactions under `enter_remote_writes` / `leave_remote_writes`), which is the pull's dominant cost on a large replay (cursor reset to 0 after a membership grant replays the whole library one transaction per row).

### 2.3 Poison row wedges an entire table forever — CONFIRMED, tier-1

`pull_table` treats every non-`RemoteDeleteRefused` error as "stop this table, hold cursor, retry next cycle" (`pull.rs:405-412, 451-457, 487-497`). There is no per-row skip, dead-letter, or surfacing beyond `eprintln!`. Because pages are ordered by `sync_seq`, a single row that fails deterministically blocks every later row in that table, and `pull_all` then defers every child table (`pull.rs:233-244`). Deterministic failures that exist today:

- `apply_server_head` on a document whose live projection diverged from its optimistic head (`remote_sync.rs:236-240`, the "bricked doc" from `project_authored_state_relational.md`) → **every** document's server head stops arriving on that device.
- An immutable row whose local copy differs by one byte (`verify_row_except`, `pull.rs:1233-1270`) → `authored_revisions` stops, and with it files, parents, heads, proposals, integrations, archives, turn preparations, outcomes, messages.
- A `MissingField` / `Parse` error from a schema mismatch (§2.10).

Failure scenario: one corrupt document → the device silently stops receiving any authored change from any other device, forever, with no UI signal. `PullStats.errors` is returned from `sync_full` but the background loop only prints it (`push.rs:709`).

Minimal fix: per-table poison counter in `sync_state` (or a `pull_failures` table keyed by `(uid, table, sync_seq)`); after N identical failures on the same `sync_seq`, record the row as quarantined, advance past it, and emit a `sync-quarantine` event. Size: ~150 lines in `pull.rs` + one migration + one event. The row is still in history on the server, so quarantine is not data loss.

### 2.4 `mark_synced` overwrites a newer local edit — CONFIRMED, tier-1

Sequence (all in `push::flush_pending_with_integrator`):

1. `enqueue_dirty` snapshots row R at version v as the op payload (`orchestrator.rs:298-312`).
2. `execute_op` awaits the remote upsert (`push.rs:231-234`).
3. During that await the user edits R: trigger sets `synced_at = NULL, version = v+1`.
4. Remote returns OK → `mark_synced` runs `UPDATE R SET synced_at = updated_at, version = version + 1` with no version guard (`registry.rs:186-191`, `push.rs:519-567`).

R is now clean locally but the remote has the pre-edit payload. Nothing re-pushes it until the next unrelated local edit; nothing re-pulls it because the remote `sync_seq` for R was produced by *this* push and the pull skips… actually it does not skip: the pull will bring the *stale* remote row back and overwrite the newer local edit (`is_locally_dirty` is false because `synced_at` is set). Net effect: the user's edit is reverted on the next pull, silently. Likely during rapid edits (slider drags, arg tweaks) on any `DirtyUpsert` table.

Minimal fix: carry `version` in the op payload (or a `dirty_version` column) and make `mark_synced_sql` `... WHERE pk AND version = ?`; on 0 rows affected, leave the op removed but do not mark clean (the row is still dirty and will re-enqueue). ~30 lines in `registry.rs`, `orchestrator.rs`, `push.rs`. Also fixes the same race in `pull.rs:368-376` → `execute_upsert` (dirty check outside the write transaction; move the `synced_at IS NULL` check into the `BEGIN IMMEDIATE` via `... ON CONFLICT DO UPDATE ... WHERE synced_at IS NOT NULL`).

### 2.5 `enqueue_dirty` defeats backoff and dead-lettering — CONFIRMED, tier-2

`enqueue_upsert`'s `ON CONFLICT ... DO UPDATE SET payload_json = excluded, attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP` (`pending.rs:80-84`). `enqueue_dirty` runs every loop iteration (`push.rs:637`) and re-enqueues every `synced_at IS NULL` row. So any `DirtyUpsert` op that failed (409 unique violation, 400 schema mismatch, RLS 403) has its attempt counter zeroed every 10 s and is retried every 10 s forever. The `MAX_ATTEMPTS = 20` dead-letter (`pending.rs:26`) is unreachable for the entire catalog; it only applies to immutable/explicit ops. The comment at `pending.rs:25` and the design note "surface dead-lettered ops" both assume a mechanism that does not function for dirty rows.

Failure scenario: a client ahead of the server (§2.10) hammers PostgREST with a 400 on every dirty row every 10 s, with `[sync] push ...` log spam, indefinitely.

Minimal fix: in the `ON CONFLICT` branch only reset `attempts`/`next_retry_at` when `excluded.payload_json <> pending_ops.payload_json` (content actually changed). ~5 lines. Then §2.6 becomes reachable and must be surfaced.

### 2.6 Dead-lettering is not surfaced — CONFIRMED, tier-2

`record_failure` at `attempts >= 20` writes `last_error` and prints (`pending.rs:425-452`). `PendingOp.last_error` is `#[allow(dead_code)]` "failed-op reporting was a UI surface that no longer exists" (`pending.rs:39-42`). `list_failed` / `count_pending` are `#[cfg(test)]`. There is no event, no command, no counter. An immutable revision that dead-letters (e.g. §2.11 actor mismatch) means a document's history is permanently split across devices with no signal.

Minimal fix: emit `sync-dead-letter { table, record_id, error }` on the host `Events` from `record_failure`, and expose `list_failed` through dispatch. ~40 lines; needs `SyncHost` threaded into `flush_pending` (it already reaches `run_sync_loop`).

### 2.7 Tier / FK ordering — CONFIRMED sound, with one gap

Kahn order from `parents` (`registry.rs:858-861`), verified by `topo_order_places_parents_before_children`. Pull defers children when a parent table fails. Push sorts ops by topo position within the 1000-row window. Gaps:

- `parents` are declared by hand and never checked against the actual FK graph (SQLite `PRAGMA foreign_key_list` or Postgres). E.g. `authored_turn_preparations` lists `agent_threads` but not `agent_thread_messages` although it keys on `assistant_message_id`; `agent_thread_messages` lists `authored_turn_preparations` as a parent, so the two are mutually ordered only because of the trigger `assistant_message_requires_prepared_authored_turn`. Works, but the reason is not in the registry. PLAUSIBLE only-because-of-ordering.
- Authority ops (`table_name = 'authored_head_authority'`) sort last via `unwrap_or(usize::MAX)` (`pending.rs:382-387`). Correct, but implicit.
- Cross-cycle FK holes self-heal: a child row referencing a parent committed after the parent table's page was fetched fails locally, holds the cursor, and succeeds next cycle. That is correct but is exactly the "stop the table" path of §2.3, so a legitimate one-cycle FK lag is indistinguishable from a poison row in the logs.

### 2.8 Refused-delete / tombstone matrix — CONFIRMED, no-cascade rule holds locally, not remotely

| Table | Local tombstone action (`delete_local`) | Refusal condition (`ensure_remote_delete_is_safe`) |
|---|---|---|
| venues | hard delete | any fixture/group/override/score/cue/midi/stage/thread/authored doc references it |
| tracks | hard delete | non-stub `file_path`, album art, or any dependent analysis/score/thread/doc |
| scores | owner + authored doc → `archive_score_from_remote`; else delete only if no `track_scores`/threads/docs | |
| patterns | owner + authored doc → `archive_pattern_from_remote`; else delete only if no clips/threads/docs/cues/overrides and all implementation graphs empty | |
| everything else | hard delete, `origin` set to `'remote'` first so the delete trigger does not echo | none |

Refusals are `RemoteDeleteRefused`, logged and skipped (cursor advances) (`pull.rs:399-404`). This satisfies `feedback_no_cascade_deletes.md` locally. Two smells:

- **Refusals are silent and permanent.** A refused tombstone is never retried and never shown; the local row diverges from the remote forever. Every device that refuses keeps the row; the remote is soft-deleted. The next local edit re-pushes the row with `deleted_at` untouched (payload never includes it), so the row resurrects remotely with `deleted_at` still set — which pull then treats as a tombstone again on other devices. PLAUSIBLE ping-pong; needs a test.
- **The server side has no such rule** (*live*): RLS grants `DELETE` on every catalog table and 14 FKs are `ON DELETE CASCADE`. Soft delete is a client convention. Any client (or a curl with a user token) hard-deleting a venue cascades fixtures/groups/cues/scores on the server with no tombstone for anyone to pull. Also `cascade_soft_delete_track_children` skips `track_genres`. Tier-2, server-side: add `reject_delete` triggers mirroring the authored tables' (`sync.sql:600-603`) and add `track_genres` to the cascade. ~30 lines SQL.

### 2.9 `origin` local/remote — CONFIRMED correct, one misuse

`execute_upsert` assigns `origin = 'local'` for own rows so local deletes echo, `'remote'` otherwise (`pull.rs:675-683`). `delete_local` flips to `'remote'` before deleting (`pull.rs:899-907`). `remote_origin_is_data_not_write_authority` proves origin is not a capability. Fine.

Smell: `sync_delete_pattern_categories` has no `origin` guard and `pattern_categories` is not in the registry (schema build, `migrations/20260328300000_origin_provenance.sql:93`). Today the only delete is the sign-out wipe under maintenance mode, where the trigger's admission subquery yields no row, so nothing is enqueued. If a category delete is ever added, it enqueues an op that `execute_op` rejects as "not registered" → 20 failures → dead-letter (silent, §2.6). Tier-3: drop the trigger or register the table.

### 2.10 Registry vs schema vs deploy ordering — CONFIRMED no test, one live drift

Built the SQLite schema from `migrations/` and diffed against `registry::TABLES`: every registry column exists locally; local-only extras not synced are `venues.{controller_port,mixer_port,mixer_mapping_json}`, `tracks.{source_type,source_id,source_filename}`, `patterns.category_id`, `track_roots.logits_path`, `agent_threads.actor`. `tracks.source_id` is the stable DJ-library identity (`MEMORY.md`), so a second device loses the Rekordbox/Engine match — flag, probably intentional-by-omission.

Against Postgres (*live*): `scores.dsl_text` exists server-side and is in no registry list (dead column); the six `track_*` analysis tables have a server `id` uuid PK the client never sees (conflict key is `track_id`, works because of a UNIQUE); `tracks.album_art_path` is marked `local_only` but exists remotely. Catalog DDL is not in `supabase/migrations/` at all — only the four authored/genre migrations are — so none of this is reviewable from the repo.

**No consistency test exists** on either side. `sync/tests.rs::test_pool` hand-writes a second copy of the `venues`/`pending_ops`/`auth_write_admission` DDL instead of running migrations.

**Deploy ordering:**

- Client ahead of server (registry column not yet in Postgres): every pull of that table 400s (`select=` names an unknown column) → table + all children deferred every cycle; every dirty push 400s and is retried every 10 s (§2.5). For the current edit (`agent_threads.parent_*`, `authored_turn_preparations.workspace_id`, `authored_revisions.actor`) that is the entire authored subgraph. Server must deploy first; there is no version handshake to enforce it.
- Server ahead of client: pulls omit the column (fine, nullable), pushes omit it and `merge-duplicates` leaves it untouched (fine). Immutable tables: see §2.11.

### 2.11 `authored_revisions.actor` rollout collides with the immutable contract — CONFIRMED, tier-1 (rollout window)

Server: `actor text NOT NULL DEFAULT 'unknown'` + one-time backfill (`supabase/migrations/20260827000000_authored_revision_actor.sql`). Local: same column + same backfill at migration time.

1. Server migrates. Old client (no `actor` in payload) creates revision X → server stores `actor = 'unknown'`.
2. That client upgrades. Local migration backfills X to `'user'` (or the model key).
3. Pull returns X with `'unknown'`; `execute_upsert` → `ON CONFLICT DO NOTHING` (0 rows) → `verify_row_except` compares all columns including `actor` → mismatch → `Local("immutable remote row collided…")` → **`authored_revisions` wedges for that principal** (§2.3), on every device that had X locally before upgrading.
4. Any replay push of X (e.g. structural-merge closure re-upload in `upload_revision_closure`, which re-sends every revision it references) hits `immutable_update_or_identical` → 23505 → 409 → dead-letter after 20 (silent, §2.6).

Same shape for any future column added to an immutable table with a non-trivial default. Minimal fix: exclude `actor` from `verify_row_except` for `authored_revisions` (it is provenance, explicitly not part of identity per the migration comment) and let pull backfill `'unknown'` → local value (or vice versa) as a `ServerEnriched`-style column; ~15 lines in `pull.rs` + registry policy. Plus a server-side `updated_or_identical` exemption for `actor` if replay must succeed. This needs deciding before the branch ships.

### 2.12 409 handling — CONFIRMED correct, incomplete

`push.rs:78-101`: 409 is no longer treated as success; 23503 vs other is only a log label. Both paths `record_failure` (or `record_integration_retry`). Correct. But a 23505 on an immutable row is a *permanent* divergence, not a transient one; it should not consume 20 attempts over ~90 minutes before going quiet — it should dead-letter immediately and surface. Tier-3.

### 2.13 Principal gating — CONFIRMED push side, PLAUSIBLE gap on pull side

- Signed out: `enqueue_*` require `admission.active_uid = ?` and produce 0 rows otherwise → `AuthRequired`; delete triggers write under `'signed-out'` and `fetch_ready_ops` never returns those (test `signed_out_tombstone_never_flushes_and_survives_sign_in`). Sound.
- Push: `flush_pending_with_integrator` requires `token_user_id == admitted_user_id` (`push.rs:42-51`); every `remove_op` / `record_failure` / `mark_synced` re-checks admission in SQL, so a switch mid-batch fails closed. Sound.
- **Pull**: `run_pull_cycle` (`push.rs:669-672`) and `sync_full` (`orchestrator.rs:83`) take `uid` from the *state DB token* and never compare it with `admitted_principal(pool)`. `enter_remote_writes` checks `armed && accepting`, not `active_uid` (`write_admission.rs`). Identity switches are serialized by `sync_lock` (`auth.rs:423-425`), so the window is only a sign-in that lands between `get_auth` and the first write — the identity-switch code path takes the lock, so this is probably unreachable today. Marked PLAUSIBLE; the asymmetry with push is the smell: pull writes `origin = 'local'` for rows whose `uid` matches the token uid, and advances `sync_state` for the token uid, on the assumption they are the admitted principal. One `require_admitted(pool, &uid)` at the top of both entry points closes it. ~10 lines.

### 2.14 Head-proposal integration ordering — CONFIRMED sound as transport; lossy as merge

- Ordering is `server_proposal_seq` = commit order under the document row lock (`sync.sql:1895-1951`). `is_earliest_pending` is computed server-side; a `not_earliest` integration is non-terminal and retried (`pending.rs:491-528`). Any owner device integrates any pulled proposal. Submission requires the closure to already be on the server, so a dead device cannot wedge others. CONFIRMED.
- Two online devices integrating the same earliest proposal concurrently: both compute a structural merge and upload it; first RPC wins, second gets `stale`/`already_resolved`. The loser's merge revision stays in server history as an orphan. Harmless, but history accumulates `sync_integration` orphans under contention. Tier-3.
- **"Never conflicts" means "never reports one".** `authored_sync_merge` (`services/authored_sync_merge.rs`) resolves every overlap as *proposal wins*, and any structural/validation failure as *whole proposal* (`merge_graph_total:111-134`, `merge_track_total:80-109`). The module's own tests assert the drops (`later_delete_wins_delete_modify`, `track_merges_independent_fields_and_proposal_wins_overlap`, `cycle_from_structural_composition_falls_back_to_whole_proposal`). Concrete: device A (integrated first) lengthens clip `a` and tweaks 20 other clips; device B (later seq) deletes `a`. Result head = B's document; A's 20 unrelated edits survive only if the merged result validates; if it does not (any clip envelope failure, any graph cycle), `WholeProposalFallback` discards *all* of A's edits, not just the overlapping one. Also `remote_sync.rs:502-520`: if trivia (comments) merge fails, the whole proposal snapshot is used even though the semantic merge succeeded. A's edits remain in history (their revision is the head's parent0), so this is recoverable by manual restore, but nothing tells the user. **CONFIRMED lossy; tier-1 because it is silent.** Minimal fix is not a merge change — it is a receipt: when resolution is `WholeProposal` or a field overlap was taken, write an `authored_operation_outcomes` row with `conflicts_json` describing what current lost, and surface it in the history UI. The merge engine already knows every overlap it resolved; it just does not return them. ~100 lines in `authored_sync_merge` (return a `Vec<Overlap>` beside the value) + outcome row.
- `merge_base` `NotFound | AmbiguousMergeBase` → `base = None` → `WholeProposalFallback` unconditionally (`remote_sync.rs:461-466, 677-681, 702`). Criss-cross histories (two structural merges of each other) produce ambiguous bases; after that, every later proposal replaces the head wholesale. PLAUSIBLE, tier-1 magnitude; needs a two-device criss-cross test.

### 2.15 Server-head → local CAS during pull — CONFIRMED safe, transiently regressive

`apply_server_head_observation` reads the local head outside a transaction, re-reads inside `BEGIN IMMEDIATE`, and errors with `HeadConflict` if it moved (`remote_sync.rs:148-170`) → pull stops for that table, cursor held, retried next cycle. No silent overwrite. Two consequences:

- If the local head moved because of an optimistic local edit, the *next* successful application rewrites the live projection to the server revision and CASes the head to it (`remote_sync.rs:232-282`). The user's edit disappears from the editor until its proposal integrates (seconds to a minute), then reappears merged. Visually it is a flicker-revert; in the DAG it is fine. Tier-3 UX, worth a note in the history UI ("syncing…").
- The head row fetched at page time can be stale by the time it is applied (server moved again). Local head regresses to the older server revision, then advances on the next pull. Same flicker. PLAUSIBLE.
- The `HeadConflict` error is one of the §2.3 stop conditions; a document whose head moves on every cycle (active editing) could starve the pull of every other document's head. PLAUSIBLE under heavy local editing; the fix is to make head application per-document retryable inside the page rather than a table-level stop.

---

## 3. Design review (Ousterhout)

**Where the complexity lives.** `pull.rs` (1430 lines of code + 1500 of tests) is the deep module and it is the right place: registry-driven SQL, six pull policies, tombstone safety, per-table special cases. But it has absorbed things that are not "pull": archive reconciliation, thread-deletion recovery, agent transcript head observation (`apply_agent_transcript_head_observation` is called from *push*). The seam between "transport" and "authored domain" exists (`HeadProposalIntegrator`) for integration but not for head projection — `pull_table` branches on `table.name == "authored_document_heads"` and `"authored_head_integrations"` by string (`pull.rs:419, 463`). That is change amplification: adding a server-authoritative projection means editing `pull_table`, `registry::pull_policy`, `push_policy`, `has_remote_tombstone`, `is_locally_dirty`, and `build_upsert_sql`.

**Duplicated contracts (the big one).** The column list for every synced table exists in four places with no consistency test: `registry.rs`, `src-tauri/migrations/`, `supabase/migrations/` (authored tables only), and the live Postgres schema (catalog tables, not in git at all). The pull policy enum in `registry.rs` duplicates the per-table trigger set on the server (`immutable_update_or_identical`, `preserve_catalog_tombstone`, `guard_*`). Nine string-grep tests in `authored_remote.rs:412-609` try to hold the Postgres side still by matching literal SQL fragments — that is a contract test written as a regex, brittle in both directions.

**Two merge engines.** `authored_merge.rs` (2285 lines, symmetric, typed, reports conflicts) and `authored_sync_merge.rs` (584 lines, asymmetric, `serde_json::Value`, resolves by policy) implement the same three-way ladder — "equal → current; current == base → proposal; proposal == base → current; else *X*" — with *X* = report vs pick-proposal. Clip/edge canonical sorts are copied verbatim. The sync engine also lacks the semantic-dependency and edge-cascade rules the workspace engine has, which is precisely why it falls to whole-proposal so often (§2.14). One kernel parameterized by conflict policy is the design; the sync variant would then return the overlaps it resolved for free.

**Pass-throughs.** `supabase_remote.rs` is a 100-line `RemoteClient` impl whose entire body is `map_err(convert_err)` between two error enums (`database::remote::common::SyncError` and `sync::error::SyncError`) that model the same three cases. That is the 1:1 wrapper-error-per-layer anti-pattern named in CLAUDE.md. Collapse: make `common.rs` return `sync::error::SyncError` directly (or delete `common::SyncError`). `SyncEngine::pool/state_pool/remote/authored` getters exist so `lib.rs` can pass the fields back into `run_sync_loop` — the loop should be a method on `SyncEngine`.

**Shallow interfaces.** `run_sync_loop` takes 8 parameters (`push.rs:571-580`), every one a field of `SyncEngine`. `execute_op` takes 6. `pull_all` takes 7 including two host stores that exist only so `recover_deleting_agent_threads` can be called at the end — a pull that also runs thread cleanup is two responsibilities in one signature. `SyncHost` already exists as the "what sync needs from the host" object (`host.rs`); `workspaces`/`graph_runs` should travel inside it, not beside it.

**Errors that hide causes.** `SyncError::Local(String)` wraps `sqlx::Error`, `AuthoredDocumentsError`, admission errors, and merge errors as text (`error.rs:52-56`, `remote_sync.rs:998-1000`). Push branches on `SyncError::Api{status}` and `Network` but every domain failure is `Local(text)`, so the flush loop cannot distinguish "retry" from "never" — which is why 23505 burns 20 attempts (§2.12) and why pull cannot quarantine (§2.3). `SyncError` needs a `Permanent` vs `Transient` axis (or a `#[from] AuthoredDocumentsError` variant with a `is_permanent()`).

**Caller-must-call-A-before-B.** `sync_files_unlocked` must run before `run_push_unlocked` so `storage_path` updates are in the dirty scan (`orchestrator.rs:137-139` comment). `bootstrap_live_projections` must run before `enqueue_dirty` (`push.rs:632-639`, `orchestrator.rs:203-211`) — duplicated at both call sites. `enqueue_pending_head_integrations` must run after every table is pulled or it enqueues integrations for proposals whose closures are absent. None of these are encoded; all are comments.

**Temporal decomposition.** `discover_venues` is a special-cased mini-pull for `venues` + `venue_members` with its own upsert SQL (`pull.rs:174-214`), because `venue_members` is not in the registry and visibility must be established before the registry pull. It writes `venues` rows that the registry pull will write again. A `Membership` pull policy would fold it in.

**Obscurity.** `sync_state.last_pulled_at` stores `seq:<n>`. `pull.rs` line 1 still says "delta pull"; `mod.rs` says "commit-ordered server sequence cursors" — the module docs disagree with the memory notes, and the column name lies. One migration renaming the column to `last_seq INTEGER` removes the `CASE WHEN substr(...)='seq:'` SQL in `state.rs:50-62`.

---

## 4. Test coverage

60 tests across `sync/tests.rs` (25), `pull.rs::remote_deletion_tests` (16), `authored_remote.rs` (9), `registry.rs` (2), `state.rs` (2), plus the authored-documents integration tests. What they cover well: pending-queue principal scoping, sign-out/sign-in queue survival, cursor monotonicity, venue discovery and cursor reset, tombstone refusal rules for scores/patterns/tracks, thread deletion receipts, proposal receipt replay, archive convergence.

Against §2:

| Scenario | Status |
|---|---|
| Cursor held back on mid-page failure (§2.3) | NOT COVERED — no test drives `pull_table` with a failing row |
| Poison row blocks table + children | NOT COVERED |
| `mark_synced` vs concurrent edit (§2.4) | NOT COVERED |
| `enqueue_dirty` resets attempts (§2.5) | NOT COVERED (`test_retry_backoff` stops at attempts=1) |
| Dead-letter at 20 (§2.6) | NOT COVERED |
| Pull page partial failure / resume | NOT COVERED |
| Child deferred when parent fails (§2.7) | NOT COVERED |
| Refused tombstone advances cursor, never retried (§2.8) | PARTIAL — `delete_local` refusals tested; `pull_table` skip path not |
| Origin assignment on pull | NOT COVERED |
| 409 FK vs unique, 401 abort | PARTIAL — 23505 only |
| Principal switch mid-run (§2.13) | PARTIAL — pre/post switch only |
| Two-device out-of-order proposals rebased on each other (§2.14) | PARTIAL — scripted remote only |
| Sync merge drops current-side edits | COVERED *as intended behavior*; no test that the loss is recorded or restorable |
| Server-head CAS race (§2.15) | PARTIAL — replace path only; `HeadConflict` and diverged-projection error untested |
| Registry vs SQLite schema (§2.10) | NOT COVERED (this audit did it by hand) |
| Deploy ordering / unknown column | NOT COVERED |
| `actor` immutable collision (§2.11) | NOT COVERED |

**Top 5 missing tests** (each ~50–120 lines against the existing `SequenceRowsRemote` / `migrated_pool` fixtures):

1. `registry_columns_match_migrated_sqlite_schema` — `PRAGMA table_info` for every `TableMeta`; assert every registry column exists and every non-`local_only` column not in `{synced_at, origin, version, deleted_at, role}` is listed or explicitly allow-listed. Kills the drift class in §2.10 on the local side.
2. `mark_synced_does_not_clean_a_row_edited_during_push` — mock remote whose `upsert_json` performs a local UPDATE on the row before returning; assert `synced_at IS NULL` afterwards. Reproduces §2.4.
3. `poison_row_holds_cursor_and_defers_children_then_quarantines` — `SequenceRowsRemote` with an `authored_revisions` row that fails `verify_row_except`; assert cursor stays, children deferred, and (after the fix) quarantine after N cycles with an event. Reproduces §2.3.
4. `redirty_does_not_reset_backoff_for_unchanged_payload` — fail an upsert, run `enqueue_dirty` twice, assert `attempts` and `next_retry_at` are untouched; then edit the row and assert they reset. Reproduces §2.5.
5. `criss_cross_proposals_record_lost_current_edits` — two devices, two structural merges, then a third proposal with ambiguous base; assert the head is the whole proposal **and** an `authored_operation_outcomes` row (or equivalent) names the dropped clips. Reproduces §2.14 and pins whatever receipt is chosen.

---

## 5. Ranked findings

### Tier 1 — data loss or permanent divergence

| # | Finding | Status | Where | Minimal fix | Size |
|---|---|---|---|---|---|
| T1.1 | Poison row wedges the whole table (and every child table) forever, silently | CONFIRMED | `pull.rs:405-412, 451-457, 487-497`; `remote_sync.rs:236-240` | Per-`(uid, table, sync_seq)` failure counter → quarantine + advance + event after N | ~150 lines + migration |
| T1.2 | `mark_synced` cleans a row edited during the push; next pull reverts the edit | CONFIRMED | `registry.rs:186-191`, `push.rs:519-567`, `orchestrator.rs:298-312` | Version-guard `mark_synced_sql`; carry `version` in payload | ~30 lines |
| T1.3 | `actor` rollout: old-client revisions (`'unknown'`) collide with local backfill → `authored_revisions` pull wedges (via T1.1) and replays 409 | CONFIRMED (rollout window) | `supabase/migrations/20260827…`, `pull.rs:703-704, 1233-1270` | Exclude `actor` from `verify_row_except` / make it server-enriched; decide before shipping the branch | ~15 lines + policy |
| T1.4 | Sync merge silently drops current-side edits (field overlap, delete/modify, and *all* edits on whole-proposal fallback, incl. trivia-merge failure) | CONFIRMED lossy | `authored_sync_merge.rs:80-134, 163-240, 427`; `remote_sync.rs:502-520, 702-712` | Return resolved overlaps from the merge; write an outcome row with `conflicts_json`; surface in history | ~100 lines |
| T1.5 | Ambiguous/missing merge base → unconditional whole-proposal replacement after any criss-cross | PLAUSIBLE | `remote_sync.rs:461-466, 677-681, 702` | Pick a deterministic base among candidates (e.g. lowest `authored_at`) instead of `None`; test | ~40 lines + test |
| T1.6 | Server permits hard `DELETE` + 14 `ON DELETE CASCADE` FKs on catalog tables; soft delete is client convention only | CONFIRMED (*live*) | live RLS/`pg_constraint`; not in repo | `reject_delete` triggers on catalog tables; commit catalog DDL to `supabase/migrations/` | ~30 lines SQL + a dump |

### Tier 2 — silent staleness or wedging

| # | Finding | Status | Where | Minimal fix | Size |
|---|---|---|---|---|---|
| T2.1 | `enqueue_dirty` zeroes `attempts` every 10 s; backoff and dead-letter never apply to catalog rows | CONFIRMED | `pending.rs:80-84`, `push.rs:637` | Reset only when `payload_json` changed | ~5 lines |
| T2.2 | Dead-letters and pull errors are `eprintln!` only; `last_error` is dead code | CONFIRMED | `pending.rs:39-42, 425-452`; `push.rs:709` | `sync-dead-letter` / `sync-pull-error` events + dispatch `list_failed` | ~40 lines |
| T2.3 | Refused tombstones are never retried and never shown; re-push of the kept row may ping-pong with `deleted_at` | CONFIRMED skip / PLAUSIBLE ping-pong | `pull.rs:399-404`; push payload omits `deleted_at` | Record refusals; include `deleted_at: null` explicitly on re-push, or clear it server-side on owner upsert | ~30 lines + test |
| T2.4 | Client-ahead-of-server deploy 400s every pull of the table and every push, forever | CONFIRMED | §2.10 | Server-first deploy discipline + a `sync_schema_version` handshake (one RPC) | ~60 lines |
| T2.5 | Pull entry points do not check `admitted_principal` against the token uid (push does) | PLAUSIBLE | `push.rs:669-672`, `orchestrator.rs:83` | `require_admitted(pool, &uid)` at both | ~10 lines |
| T2.6 | `cascade_soft_delete_track_children` skips `track_genres` | CONFIRMED (*live*) | live trigger | Add table to trigger | 2 lines |
| T2.7 | Head-application `HeadConflict` under active editing is a table-level stop, can starve other documents' heads | PLAUSIBLE | `remote_sync.rs:156-170` via `pull.rs:451-457` | Retry head application per document inside the page before stopping | ~20 lines |
| T2.8 | 23505 on immutable rows burns 20 attempts (~90 min) before going quiet; permanent divergence treated as transient | CONFIRMED | `push.rs:78-101` | `SyncError` permanent/transient axis; dead-letter immediately on immutable 23505 | ~40 lines |

### Tier 3 — design debt

| # | Finding | Status | Where | Minimal fix | Size |
|---|---|---|---|---|---|
| T3.1 | Four uncoordinated column-list copies, no consistency test; catalog DDL absent from git | CONFIRMED | §2.10 | Test #1 above; `supabase db dump --schema public` committed | ~80 lines + dump |
| T3.2 | Two merge engines with the same ladder; sync variant lacks edge-cascade / semantic-dependency rules | CONFIRMED | `authored_merge.rs`, `authored_sync_merge.rs` | One kernel, `ConflictPolicy::{Report, ProposalWins}` | ~1 day |
| T3.3 | `supabase_remote.rs` + `common::SyncError` is a 1:1 wrapper layer | CONFIRMED | `supabase_remote.rs:11-18` | Return `sync::error::SyncError` from `common.rs` | ~50 lines removed |
| T3.4 | `run_sync_loop` 8 params / `pull_all` 7 params; `workspaces`/`graph_runs` belong in `SyncHost`; loop should be a `SyncEngine` method | CONFIRMED | `push.rs:571-580`, `pull.rs:220-228`, `lib.rs:341-356` | Move fields; delete getters | ~60 lines |
| T3.5 | `SyncError::Local(String)` erases cause; no permanent/transient distinction | CONFIRMED | `error.rs`, `remote_sync.rs:998` | `#[from] AuthoredDocumentsError` + `is_permanent()` | ~40 lines |
| T3.6 | `pull_table` string-matches table names for server-authoritative projections | CONFIRMED | `pull.rs:419, 463`, `registry.rs:99-109` | `PullPolicy::ServerHead { apply: fn }`-style hook, or route via `HeadProposalIntegrator` | ~80 lines |
| T3.7 | Implicit A-before-B: files before push, bootstrap before enqueue, integrations after full pull | CONFIRMED | `orchestrator.rs:137-139, 203-211`; `push.rs:632-639`; `pull.rs:283` | Single `SyncEngine::cycle()` that owns the order; delete the duplicate call sites | ~40 lines |
| T3.8 | `discover_venues` is a hand-written mini-pull outside the registry | CONFIRMED | `pull.rs:28-214` | `Membership` pull policy for `venue_members`; let registry pull `venues` | ~100 lines |
| T3.9 | `sync_state.last_pulled_at` stores `seq:<n>`; `CASE WHEN substr` guard in SQL | CONFIRMED | `state.rs:38-72` | Migration to `last_seq INTEGER` | ~30 lines + migration |
| T3.10 | `sync_delete_pattern_categories` has no origin guard and targets an unregistered table | CONFIRMED | `migrations/20260328300000_origin_provenance.sql:93` | Drop trigger | 1 line |
| T3.11 | `sync_seq_bump` on every UPDATE incl. exact replays; `library-changed` fires on no-op re-pulls | CONFIRMED (*live*) | `sync.sql:54-64` | `WHEN (OLD.* IS DISTINCT FROM NEW.*)` on the trigger | 1 line SQL |
| T3.12 | Nine regex-over-SQL contract tests in `authored_remote.rs` | CONFIRMED | `authored_remote.rs:412-609` | Replace with a `pg_prove`/local-Postgres behaviour test or delete | — |
| T3.13 | `tracks.source_id` (DJ-library identity) not synced | CONFIRMED | `registry.rs:276-298` | Decide; add to columns if intended | 3 lines |
| T3.14 | Orphan `sync_integration` revisions from concurrent integrators | PLAUSIBLE | `remote_sync.rs:731-762, 899-909` | Accept; or defer closure upload until after a `not_earliest`/`stale` check | — |
| T3.15 | `enqueue_dirty` silently skips rows whose `read_record_as_json` fails (`if let Ok`) | CONFIRMED | `orchestrator.rs:298, 327` | Log + count | 4 lines |

### Suggested order

T2.1 (5 lines, unblocks the whole retry design) → T1.2 (30 lines) → T1.3 (must be settled before the branch merges) → T1.1 + T2.2 together (quarantine needs the event) → T1.4 (receipt) → T1.6 + T2.6 (server side, independent) → the tier-3 seam work, starting with T3.5 because T1.1 and T2.8 both want the permanent/transient axis.
