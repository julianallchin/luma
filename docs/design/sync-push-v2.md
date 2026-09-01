# State-based push (`sync-push-v2`)

Replaces the `pending_ops` journal with a push that reads the local tables at
flush time. Local-engine only: no remote migration, no RLS change, no change to
the deployed PostgREST contract. Pull, topo tiers, `origin='local'/'remote'`,
and `auth_write_admission` survive unchanged.

Companion to `docs/design/sync-audit.md`; §5 of that document is the bug list
this design is answering.

---

## 1. The flaw, stated once

`pending_ops` is a second copy of state the tables already hold. SQL triggers
write it, Rust consumes it, and every recent push bug is the two copies
drifting:

| symptom | the drift |
|---|---|
| orphaned upsert wedge | queued upsert outlives the row it copied |
| upsert/delete race | two op rows under different `op_type`, both "true" |
| stale payload snapshot | payload frozen at enqueue, row edited during the await (audit T1.2) |
| attempts reset every 10 s | re-enqueue of an unchanged row zeroes retry state (audit T2.1) |
| unpushable orphans | queue entries for rows whose parent never existed remotely |

Pull was cured of the same disease by deriving everything from ground truth (a
server-assigned `sync_seq` cursor instead of a client-clock keyset). Push gets
the same cure.

**Invariant.** *What push sends is a pure function of the local tables at flush
time. Nothing else records what to send.*

Two corollaries the implementation must never break:

- No payload is ever stored outside the row it describes.
- No intent ("please upsert X") is ever stored. Only *facts*: the row, its
  delivery marker, and — for a row that no longer exists — a tombstone.

---

## 2. The dirty predicate

Every push-relevant table answers "does this row still owe the server
something?" from its own columns. `registry::PushPolicy` gains that predicate
and the delivery that follows it:

| policy | tables | dirty predicate | delivery | receipt |
|---|---|---|---|---|
| `DirtyUpsert` | the 20 library tables | `synced_at IS NULL OR synced_at <> updated_at` | PostgREST upsert of the row read now | `synced_at = updated_at, version = version + 1` |
| `ExplicitUpsert` | `agent_threads` | same (new local `synced_at`) | upsert | `synced_at = updated_at` |
| `ExplicitImmutable` | the 10 trace/history tables | `synced_at IS NULL` (new local `synced_at`) | `insert_immutable` | `synced_at = CURRENT_TIMESTAMP` |
| `ServerAuthority` | `authored_head_proposals` | `server_proposal_seq IS NULL` | `submit_authored_head_proposal` RPC | the RPC receipt writes the seq |
| `ServerAuthority` | `authored_document_archives` | `server_archive_seq IS NULL` | `archive_authored_document` RPC | receipt writes the seq |
| `ServerAuthority` | *(virtual)* pending integrations | proposal has a seq ∧ no `authored_head_integrations` row ∧ document not archived | `integrate_head_proposal` | the integration row appears |
| `ServerAuthority` | heads, transcript heads | never dirty | — | — |

Three notes the code cannot say for itself:

**Why `synced_at <> updated_at` and not `> `.** Both sides are the *same
column value* copied by the receipt (`synced_at = updated_at`), so equality is
exact and format-independent. A `>` comparison would be a string comparison
between two different timestamp formats — SQLite's `CURRENT_TIMESTAMP` is
`'YYYY-MM-DD HH:MM:SS'` while the dirtiness triggers write
`'YYYY-MM-DDTHH:MM:SSZ'`, and `'T' > ' '`, so `updated_at > synced_at` would
report *every* row dirty forever the moment anything stamped `synced_at` from
`CURRENT_TIMESTAMP`. Inequality is the honest test: "the row moved since the
last receipt."

**The second clause is not redundant.** The dirtiness triggers set
`synced_at = NULL`, so clause one catches the normal case. Clause two catches a
trigger that forgot to: `track_genres_updated_at`
(`migrations/20260813000000_track_genres.sql:31`) bumps `updated_at` and
`version` but never nulls `synced_at`, so **local edits to `track_genres` have
never pushed**. Deriving from the row rather than from what a trigger
remembered to enqueue fixes that class by construction.

**The receipt names the row it came from.** `synced_at`'s one-second
resolution is not fine enough to notice an edit that lands in the same second as
the push, so the receipt carries a stamp guard: `... WHERE pk AND version = ?`
for the tables that have a version, `AND updated_at IS ?` for `agent_threads`,
which has none. If the row moved while the request was in flight the receipt
matches nothing, the row stays dirty, and the next cycle sends the newer
content. That, not the predicate alone, is what closes audit T1.2 — the
predicate would have missed a same-second edit.

**The authority rows were already derivable.** `SubmitHeadProposalInput` is a
column-for-column copy of the `authored_head_proposals` row that is inserted in
the same transaction (`services/authored_documents/operations.rs:660-694`);
`ArchiveAuthoredDocumentInput` likewise mirrors `authored_document_archives`
(`catalog.rs:1190-1230`); the integration wake-up is exactly the query
`enqueue_pending_head_integrations` already runs (`remote_sync.rs:293-321`).
Queueing them was copying a row next to itself.

---

## 3. Reachability replaces the FK 403

A dirty row is skipped this cycle — with no recorded state — when a registered
parent it points at is absent locally or not yet pushed. That is the whole
mechanism; the FK-403 / "unpushable orphan" class dies by construction, and a
skipped row costs nothing because the scan is re-derived next cycle.

This needs the one thing the registry did not carry: *which local column* holds
the parent key. `TableMeta::parents` becomes `&[Parent]`:

```rust
pub struct Parent {
    pub table: &'static str,
    /// The local column holding the parent's single-column primary key, when
    /// this edge is a real foreign key. `None` means the edge exists only to
    /// order the topological sort — `authored_document_heads` is ordered after
    /// `authored_revision_files` without holding a reference to one.
    pub via: Option<&'static str>,
}
```

One list, not two: the topological sort keeps reading `table`, reachability
reads `via`. This closes half of audit T2.7 (the parent list is now explicit
about which entries are FKs and which are ordering hints) and leaves the other
half open (nothing checks the list against `PRAGMA foreign_key_list`).

A `NULL` in a `via` column means "no parent", not "missing parent" — nullable
FKs (`cues.pattern_id`, `scores.venue_id`) do not block.

---

## 4. Permanently unpushable rows

`venues.id` is `TEXT` locally and `uuid` remotely. Harness debris
(`djtable-scratch-*`, `99999999-aaaa-…`) has been failing PostgREST forever and
sitting dead-lettered in the queue. The push boundary now asks one question
before delivery — `push::unpushable_reason(table, pk_values)` — and a row that
fails it is recorded `permanent` and never retried. Its whole subtree goes quiet
for free through reachability (§3), which is the only reason this narrow check
is worth having.

The function starts with the one known poison (`venues.id` must parse as a
UUID) and is the single place to extend. It is deliberately not a per-table
`uuid_keys` list in the registry: a 32-entry hand-maintained list of remote
column types is precisely the drift the audit's T3.1 is about.

---

## 5. Delete lifecycle — designed twice

The owner's ruling was `deleted_at` on every synced table locally, delete =
update. That is design A below. I implemented design B and am flagging the
deviation loudly, because A has no honest seam in this codebase; the ruling
anticipated exactly this ("if there's no good seam, that's a design finding to
write up, and scoping soft-delete to the synced-write layer with immediate-hide
semantics is acceptable if you can keep the invariant airtight").

### Design A — soft delete on the row

`deleted_at` on all 20 library tables; `DELETE` becomes `UPDATE`; the row lives
until its tombstone is confirmed, then is GC'd (or kept forever, mirroring the
remote).

Why it was rejected: **there is no seam that hides the row.** The app reads
these tables from roughly 200 hand-written SQL sites across `database/local/*`,
`services/*`, `dispatch/handlers/*`, and the render engine. The three candidate
seams all fail:

- *Views* — rename `venues` → `venues_base` and expose a filtering view. Views
  cannot be the target of a foreign key, and every venue table's FKs and its
  `auth_venue_*` admission triggers (`20260802600000`, `20260829000000`,
  `20260902000000`) are attached to the base table. Writes would need `INSTEAD
  OF` triggers on 20 views, and the admission model would have to be rebuilt on
  top of them.
- *A query layer* — none exists; there is no builder to inject a `WHERE` into.
- *Hand-editing the read paths* — 200 clauses, each of which is silently wrong
  if missed, with the failure mode "deleted venue reappears in the picker".

So A trades a bounded, mechanical write-path change for an unbounded read-path
audit whose misses are invisible. That is the wrong trade.

### Design B — hard delete plus a tombstone ledger (chosen)

The row leaves the table, so **every read path stays correct by construction**
— zero query edits, zero missable sites. The deletion *fact* — the one piece of
truth the tables can no longer hold, precisely because the row is gone — is
recorded in a ledger with no payload, no `op_type`, and no retry state:

```sql
CREATE TABLE sync_tombstones (
    principal_key TEXT NOT NULL,
    table_name    TEXT NOT NULL,
    record_id     TEXT NOT NULL,   -- registry::record_id(), U+001F separated
    deleted_at    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (principal_key, table_name, record_id)
);
```

This is not `pending_ops` in a smaller hat. It stores a fact, not an intent; it
cannot drift from the row because there is no row; and it can hold at most one
statement per identity, so the upsert/delete pair that produced the wedge is
*unrepresentable*.

**Final state wins.** At flush time push reads ground truth in one shot:

| row present & dirty | tombstone present | action |
|---|---|---|
| yes | no | upsert |
| no | yes | tombstone (PATCH `deleted_at`) |
| yes | yes | **upsert**; drop the tombstone — a present row means "not deleted", so the tombstone is stale debris from a delete-then-recreate |
| no | no | nothing |

**GC:** the tombstone row is deleted when the remote PATCH succeeds. Local keeps
nothing; the remote keeps `deleted_at` forever, which is where the convention
belongs — the remote needs it so *other devices* can learn about the delete, and
this device already knows by the row's absence. Keeping confirmed tombstones
locally would be a growing table nothing reads.

### One mechanism, and an ABORT that enforces it

Design B's real risk is a 21st delete site that forgets to record a tombstone —
today's `sync_delete_*` triggers catch that automatically. So:

1. **One function.** `database::local::sync_delete::delete_synced_where(tx,
   table, where_sql, binds)` (and its by-key twin `delete_synced_row`) records
   the tombstone and deletes the row, walking
   registered children in reverse topological order first, using the §3 FK
   links. Every hard-delete site of a synced table calls it. Because the child
   walk is explicit and registry-driven, it does not depend on
   `PRAGMA foreign_keys` or on `recursive_triggers` — the "some venue deletes
   run with FKs off and orphan the children" class stops being possible rather
   than being caught later.
2. **A guard, not a producer.** Each of the 15 tables that has a
   `sync_delete_*` trigger today gets it replaced by
   `guard_unrecorded_delete_<table>`: same `WHEN` predicate
   (`OLD.origin = 'local'` and admission armed / accepting / `maintenance = 0`
   / `remote_writes = 0`) but the body is
   `SELECT RAISE(ABORT, 'unrecorded delete of a synced row')` unless the
   tombstone is already there. It writes no sync state, so "SQL creates, Rust
   resolves" is gone; it only makes the error impossible to commit.

The `WHEN` predicate is copied verbatim from the retiring triggers, so the
tombstone surface is *exactly* today's: deletes under `enter_maintenance` (the
sign-out projection wipe, `archive_score`) and under `enter_remote_writes`
(`pull::delete_local`) still emit nothing, and `agent_threads` still propagates
as an `agent_thread_deletions` receipt rather than a tombstone. `registry::
has_remote_tombstone()` is the single list that decides which tables tombstone
at all, which drops the unregistered `pattern_categories` trigger — audit T3.10,
closed for free.

---

## 6. Retry state

The one piece of genuinely new state: attempts, last error, and next retry are
not derivable from anything.

```sql
CREATE TABLE sync_push_failures (
    principal_key TEXT NOT NULL,
    table_name    TEXT NOT NULL,
    record_id     TEXT NOT NULL,
    -- Which of the two things that can be owed for one identity failed.
    subject       TEXT NOT NULL CHECK (subject IN ('row', 'tombstone')),
    attempts      INTEGER NOT NULL DEFAULT 0,
    last_error    TEXT,
    next_retry_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- The row `version` observed when the failure was recorded. A later
    -- version is different content and starts the budget over; NULL means the
    -- subject has no version (immutable rows, tombstones) and therefore never
    -- resets.
    seen_version  INTEGER,
    -- Delivery can never succeed as-is: a non-UUID key, an immutable identity
    -- that collided with different bytes, or an exhausted attempt budget.
    -- Skipped and quiet until the content changes.
    permanent     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (principal_key, table_name, record_id, subject)
);
```

**`subject` is load-bearing.** A row and its deletion are two different things
that can be owed for one primary key, and they must not share a budget. Without
the discriminant, a deletion the server refuses twenty times hands its
`permanent` verdict to whatever row next occupies that key — and because a
tombstone failure records `seen_version` as NULL, the "content changed" escape
can never clear it, so the identity is dead forever. Every read of this table is
scoped by subject: the dirty scan joins `subject = 'row'`, the tombstone scan
joins `subject = 'tombstone'`.

Two more rules keep a verdict from outliving what it was about: a tombstone
whose row exists again is retracted *before* the retry gate is consulted, taking
its failure row with it, and sign-out deletes this principal's rows here and in
`sync_tombstones` — abandoned state is a session's, not the database's.

Keyed on `(table, pk)` and *only* touched by failures, so the audit's T2.1 —
re-enqueue zeroing the backoff every 10 s — cannot happen: the scan never
writes here. `seen_version` is what makes "content changed, try again" a fact
rather than a payload comparison.

`SyncError` gains one variant, `Unpushable`, so the boundary can tell "never"
from "not yet" without reading a message — the permanent/transient axis audit
T2.8 asks for, in the one place that currently needs it. Classification:

- transient (network, 5xx, FK 409 while a parent is in flight) → attempts + 1,
  `min(5·2^n, 300)` s backoff, same as today.
- permanent (unpushable identity §4, immutable 23505, **403**, unregistered
  table, attempts ≥ 20) → `permanent = 1`, no further attempts. A 403 is
  row-level security refusing this principal for this row; reachability has
  already removed the "parent had not landed" case, so what is left is an
  ownership disagreement the same token loses again every ten seconds.
- integration wake-ups keep today's rule: capped backoff, never permanent —
  a stale head or an earlier pending proposal must stay retryable.
- success → the row is deleted.

A permanent verdict means *abandoned*, and abandoned is not pending: the
sign-out durability audit excludes permanent subjects, so a scratch venue cannot
make sign-out impossible. It names them on the way out instead of hiding them.

Surfacing dead letters (audit T2.2) stays **open**: there is still no event and
no dispatch command, so `push_state::blocked()` is test-only. What changed is
that the answer is now one query against one table rather than a shape spread
across a queue.

---

## 7. Migration and transition

`migrations/20260904000000_state_based_push.sql`, DDL only:

1. `CREATE TABLE sync_tombstones`, `CREATE TABLE sync_push_failures`.
2. `DROP TRIGGER` for all 15 `sync_delete_*`; create the 15
   `guard_unrecorded_delete_*`.
3. `ALTER TABLE … ADD COLUMN synced_at TEXT` on `agent_threads` and the 10
   immutable trace tables.
4. Recreate the nine blanket `RAISE(ABORT)` immutability triggers with one
   exemption: an update that changes `synced_at` *and* runs inside a sync-owned
   write. Without it an immutable row could not record its own delivery, which
   is why that state used to live in the queue. What is actually checked is
   those two conditions and nothing else — an update that changed `synced_at`
   **and** a product column, inside a pull, would pass. No such statement
   exists (the immutable pull path is `ON CONFLICT DO NOTHING` and never
   updates), and tightening it further means enumerating every product column
   of nine tables in the trigger, a fourth copy of the schema for a case no
   code can reach. The admission read is `COALESCE(..., 0)`, so a database with
   no admission row refuses rather than permits.
5. `ALTER TABLE pending_ops RENAME TO pending_ops_drain`.

Pull stamps the same marker (`registry::has_delivery_marker`,
`pull::build_upsert_sql`) so a row that just arrived from the server is not
pushed straight back at it.

**The migration writes no venue row.** Venue admission triggers ABORT writes to
`venues` / `venue_nodes` / `venue_edges` / `venue_node_params` /
`venue_constraints`, which is why `20260902000000` had to rebuild those tables
rather than `UPDATE` them, and nothing here needs to: their `synced_at` is not
new, so a NULL in it is genuine.

**It does write the 11 tables that gained a marker,** and the ordering above is
why it can: their blanket immutability triggers are dropped, the rows are
stamped, and the triggers are recreated with the delivery exemption. That stamp
is the whole answer to "would an upgrade re-push all of history?", and it has to
happen *in the migration* rather than at the first push. The migration is the
only moment that cleanly separates "existed under the old engine, therefore
delivered when its queue entry was removed" from "created after the upgrade" —
a document written in the ten seconds before the first flush, or during a whole
offline session, is new, and presuming it delivered would lose it in silence.

The drain then clears the marker again for exactly the rows an operation still
names, which is the only part that needs the queue.

**The drain** (`sync::transition::drain_legacy_push_queue`) runs once, in Rust,
at the first push of the new engine, under `enter_remote_writes` so venue-table
`synced_at` updates are admitted:

| legacy op | translation |
|---|---|
| `delete` | one `sync_tombstones` row |
| `upsert`, `upsert_explicit`, `insert_immutable` | `UPDATE <table> SET synced_at = NULL WHERE pk` — the row is the payload now |
| the three authority ops | nothing; §2 re-derives them |
| any op with `attempts > 0` | a `sync_push_failures` row carrying its attempts, error, and dead-letter verdict |

**Retry history carries over.** A dead-lettered operation arrives dead-lettered,
not with a fresh budget of twenty — and its `seen_version` is read from the row
rather than left NULL, so a later edit can still clear the inherited verdict. A
verdict nothing can clear is the trap `subject` exists to remove.

then `DROP TABLE pending_ops_drain`. The table's absence is the completion flag,
so the drain is idempotent and self-cleaning. Dead-lettered garbage translates
like anything else and is then classified `permanent` at the push boundary
(§4) instead of being retried forever.

An upgrade cannot lose an unpushed edit: every legacy op becomes either a dirty
row or a tombstone, and a legacy op whose row is already gone was either a
tombstone (translated) or the orphaned-upsert wedge (correctly dropped).

---

## 8. Landmines this design adds

**The delete guards abort a repair migration that deletes rows.** The 14
`guard_unrecorded_delete_*` triggers refuse any delete of a synced row that did
not record a tombstone — including one issued by a future migration or repair
routine. This is the same class as the venue-admission landmine in
`MEMORY.md`, with one difference worth knowing precisely:

> The guards fire only when write admission is **armed, accepting, not in
> maintenance, and not in a remote write**, and only for rows whose
> `origin = 'local'`. A migration that runs at startup before the principal is
> admitted is unaffected, because `armed = 0` makes the `EXISTS` false. A
> repair that runs *after* sign-in — a runtime fixup, a maintenance command, a
> migration on an already-open database — is refused.

The escape hatches, in order of preference: route the delete through
`database::local::sync_delete` (it records the tombstone and the server hears
about the deletion, which is usually what you actually wanted); or wrap it in
`enter_maintenance` / `enter_remote_writes`, which is a claim that the deletion
is *not* this device's to report. There is deliberately no third option: a
delete of a synced row is either told to the server or explicitly declared not
to be.

If a migration-time arm seam is ever wanted, the cheapest one is a
`sync_guards_armed` flag on `auth_write_admission` that the guards read
alongside the admission state — but nothing needs it yet, and adding it now
would be a switch with no user.

---

## 9. What this closes, and what it does not

**Closed by construction**

| audit | finding | how |
|---|---|---|
| T1.2 | `mark_synced` cleans a row edited during the push | the payload *is* the row read at flush; the receipt is `synced_at = updated_at`, and an edit during the await moved `updated_at`, so the row stays dirty |
| T2.1 | `enqueue_dirty` zeroes attempts every 10 s | retry state is a separate table only failures write |
| T3.10 | `sync_delete_pattern_categories` targets an unregistered table | trigger deleted; `has_remote_tombstone` is the only list |
| — | orphaned-upsert wedge | no queued upsert exists to outlive its row |
| — | upsert/delete race | one identity, one ledger row, final state wins (§5) |
| — | FK 403 on an unreachable parent | reachability skip (§3) |
| — | non-UUID venue ids retried forever | permanent classification (§4) |
| — | venue deletes that orphan children on an FK-off connection | registry-driven child walk (§5) |
| — | `track_genres` never pushes local edits | second clause of the dirty predicate (§2) |
| — | `is_locally_dirty` splits composite record ids on `':'` (`pull.rs:1300`) while everything else uses U+001F | rewritten against the same predicate as the scan |
| — | sign-out's durability audit counted queue rows, so it could not see an undelivered immutable trace once the queue lost it | it now counts undelivered rows per table plus pending tombstones, and names them |

**Accepted residual**: a 403 is treated as permanent, so a *transient* RLS or
token state — a membership grant the server has not yet seen in this token's
claims — turns one row into an abandoned record that needs a content change (or
a re-created row, for a tombstone) to be tried again. The alternative is the
behaviour the audit is complaining about: the same refusal, every ten seconds,
forever, unreported. Abandoned records are named at sign-out and listed by
`push_state::blocked()`.

**Explicitly out of scope** (unchanged, still true): T1.1 poison-row pull wedge,
T1.3 `actor` rollout, T1.4/T1.5 merge loss, T1.6 server-side hard deletes, T2.2
dead-letter surfacing, T2.3 refused tombstones, T2.4 deploy ordering, T2.5 pull
admission check, T2.6 `track_genres` server cascade, T2.7 (FK list unverified),
T2.8 permanent/transient error axis in `SyncError` (this design classifies at
the push boundary instead), T3.1–T3.9, T3.11–T3.15.

**Known behaviour this design does *not* fix, found on the way**

- `venues::delete_venue` (`database/local/venues.rs:257`) runs under
  `enter_maintenance`, and the tombstone predicate requires `maintenance = 0`.
  A user deleting a venue therefore removes it locally and never tells the
  server; the next pull brings it back. The new mechanism preserves this
  behaviour exactly rather than silently changing it — it is a product
  decision, not a transport one.
- `archive_pattern_inner` with `publish = true` (`catalog.rs:1152`) emits a row
  tombstone *and* an authored archive RPC, while `archive_score` suppresses the
  tombstone. Asymmetric; preserved as-is.
