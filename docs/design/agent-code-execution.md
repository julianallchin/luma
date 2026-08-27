# Agent Python workspaces and track authoring — final design

Status: **final architecture; relational authored-state, durable conversation,
Python workspace, and track-authoring paths are the settled foundation, with
production hardening still governed by the acceptance criteria** (2026-08-02)

Scope:

- the track-editor lighting copilot;
- the pattern-editor graph agent;
- the durable agent-thread foundation both agents will share;
- the binding/artifact data plane that exposes Luma state to Python;
- the narrowly scoped host capabilities behind track authoring;
- the local Python runtime, artifacts, interruption, and sandbox boundary.

This document supersedes the earlier exploratory design. It records what was
settled, the intended architecture, the agent-facing behavior, current-code
constraints, and the acceptance criteria. An implementing agent should be able
to work from this document without needing the conversation that produced it.

---

## 1. Decision in one paragraph

Every durable agent thread owns one persistent Python workspace. The model gets
one notebook-like `python` tool. Before each cell, Luma atomically refreshes one
reserved root named `luma`; variables created by the agent remain in the kernel
namespace across calls. All track, audio, musical-feature, venue, pattern, and
graph-run data reaches Python through one versioned binding manifest and one
artifact store. Numerical data preserves its semantic axes, units, identities,
and provenance instead of crossing the boundary as anonymous arrays. In a track
thread, `luma.track` is both the complete lossless authored-track snapshot and
the entry point to one staged edit transaction. The agent changes a full local
candidate with `add_clip`, `update_clip`, and `remove_clip`, inspects it, then
asks the authoritative host to check or atomically apply it. Python receives no
database or general application authority. Graph mutation continues through
the graph agent's canonical validated interface. Scores and graphs share one
immutable relational revision DAG, CAS head, typed merge service, and Supabase
row-sync path; subagents receive only plain directories materialized from a
recorded base revision. Conversation messages form a separate immutable DAG so
a rewind can fork a transcript without rewriting it. A production build runs
Python with no network, only explicit input artifacts readable, and only thread
scratch space writable.

The important shape is:

```text
durable agent thread
    owns one Python workspace
        owns zero or one live Python kernel
        owns cell history, scratch files, and generated artifacts

current Luma state
    -> one binding assembler
    -> one immutable binding revision
    -> reserved Python object: luma
```

---

## 2. Why this exists

The north star is:

> Give agents a real code-execution environment over the audio and the lighting
> output, so one piece of code can measure what the track is doing, measure what
> the pattern emits, and correlate the two.

Before this design, the graph probe could inspect only a graph's output, not the
audio or extracted musical features that output was supposed to follow. The
track agent received several musical features only as prose in its system
prompt, including a quantized ASCII drum grid, and had to reason over text
instead of computing over exact values.

The desired capability is deliberately open-ended:

- use the already-computed beats, drum onsets, bars, chords, spectral features,
  and venue geometry;
- access the source audio mix and stems when the question is about audio rather
  than an already-derived feature;
- derive custom thresholds and new features with NumPy, SciPy, and librosa;
- inspect graph outputs as semantic spatiotemporal tensors;
- correlate musical events against lighting events;
- create diagnostic figures such as spectrograms and overlays;
- preserve helper functions and intermediate variables across an agent thread.

This is not merely a larger menu of fixed analysis tools. The model must be able
to write the analysis that the task requires.

---

## 3. Goals and non-goals

### 3.1 Goals

1. One Python execution mechanism shared by both agents.
2. One persistent workspace per durable agent thread.
3. One generic path for all Luma data entering Python.
4. Semantic numerical data: axes, coordinates, labels, units, and identities
   remain attached to values.
5. Exact, composable analysis over audio, features, venue geometry, the authored
   lighting timeline, graph definition, and graph output.
6. Notebook-native output: last expression, stdout, stderr, tracebacks, and
   figures.
7. Immutable Luma snapshots in Python, plus one explicit staged track-edit
   transaction.
8. A single coherent track-authoring surface instead of one model tool per clip
   operation.
9. Local execution with interruption, bounded output, and production sandboxing.
10. Components that are independently unit- and integration-testable.
11. Host-derived authorization, authoritative validation, and atomic apply for
    every track mutation.
12. One relational, content-addressed revision DAG for score and graph history,
    with no second authored-state authority.
13. Full authored history and immutable conversation traces backed up through
    the existing Supabase row-sync engine.
14. Plain isolated subagent directories with strict semantic merge, and silent
    total convergence for cross-device sync.
15. Forward state restore plus optional transcript-fork rewind; no history or
    message row is ever rewritten.

### 3.2 Non-goals

- Python is not a second application backend or a direct database API.
- Python does not receive SQLite handles, general Tauri callbacks, credentials,
  or arbitrary host paths. Its host-call protocol exposes only named,
  scope-bound capabilities installed by the trusted command layer.
- Python does not mutate graphs or application state through arbitrary APIs.
  The sole track exception is the explicit `luma.track.edit()` transaction.
- The executor does not invent a separate transport for every domain value.
- The executor does not promise exact serialization of arbitrary CPython heap
  state across app restarts.
- A Python program does not become the canonical persisted representation of
  an authored track.
- The current Python agent does not mutate `score.luma` directly; it uses the
  typed `luma.track` transaction. The same lossless DSL is nevertheless the
  canonical score file stored byte-for-byte in each relational revision, so a
  future filesystem agent can edit it through the same validation boundary.
- Subagent scheduling is not part of the Python executor. Its isolated
  directories, revision creation, and semantic merges are part of the shared
  authored-state system.
- Windows execution does not ship without a real native sandbox.

---

## 4. Terminology

Use these terms consistently. Avoid the unqualified word `context`, which is
ambiguous between model tokens and runtime variables.

| Term | Meaning |
|---|---|
| **Agent thread** | Durable conversation identity and structured message history. |
| **Model context** | The messages and tool history sent to the language model. |
| **Python workspace** | Thread-owned cells, artifacts, scratch files, and live-kernel association. |
| **Python kernel** | The live subprocess and its mutable Python namespace. |
| **Kernel namespace** | Variables, functions, and imports created by executed cells. |
| **Analysis scope** | IDs and time window identifying the current track, venue, score, pattern, and graph run. |
| **Luma bindings** | Host snapshots exposed under the reserved `luma` object. Bound values are immutable; selected objects may expose explicit host capabilities. |
| **Binding revision** | One immutable, internally versioned set of Luma bindings used by a cell. |
| **Track revision** | Semantic revision of the complete authored clip set used for optimistic concurrency. |
| **Track edit** | Python-local mutable candidate created from one track snapshot and base track revision. |
| **Authored document** | One score or pattern graph, identified independently of its display name and owned by one principal. |
| **Authored revision** | Immutable metadata, canonical file bytes, and zero, one, or two ordered parent edges in the relational revision DAG. |
| **Document head** | The sole mutable pointer to the current authored revision; advanced by generation compare-and-swap locally and by ordered proposal integration remotely. |
| **Isolated workspace** | Disposable plain directory materialized from a recorded base revision for a subagent; never an authority or source of history. |
| **Head proposal** | Immutable request to integrate a revision tip into the server-authoritative document head. |
| **Transcript node** | Immutable parent-linked message containing complete structured message parts; forks share nodes rather than copying or editing them. |
| **Semantic tensor** | Numerical values plus named axes, coordinates/labels, units, and provenance. |
| **Artifact** | An immutable large input or generated output referenced by opaque ID. |
| **Cell** | One invocation of the model-facing `python` tool. |

---

## 5. Prerequisite: a durable agent-thread foundation

This is a prerequisite, not incidental executor plumbing. Building a persistent
kernel on top of target-keyed React maps would create hidden ownership and reset
bugs.

### 5.1 Required thread contract

Both agents must use the same durable thread abstraction:

```rust
struct AgentThread {
    id: AgentThreadId,              // opaque UUID
    owner_user_id: Option<UserId>,  // captured by the host; None is signed-out local use
    agent_kind: AgentKind,          // TrackCopilot | PatternGraph
    subject_kind: Option<String>,   // track | pattern
    subject_id: Option<String>,
    venue_id: Option<String>,       // pinned trusted scope
    score_id: Option<String>,       // persistence identity, not an agent namespace
    title: Option<String>,
    lifecycle_state: LifecycleState, // Active | Deleting; deleting is terminal
    forked_from_thread_id: Option<AgentThreadId>,
    forked_at_message_id: Option<MessageId>,
    created_at: Timestamp,
    updated_at: Timestamp,
}

struct AgentThreadMessage {
    id: MessageId,
    owner_user_id: Option<UserId>,
    principal_key: String,
    created_in_thread_id: AgentThreadId, // immutable provenance, not ownership
    parent_message_id: Option<MessageId>,
    depth: i64,
    role: String,
    parts: JsonValue,               // complete UIMessage.parts
}

struct AgentThreadTranscriptHead {
    thread_id: AgentThreadId,
    head_message_id: Option<MessageId>,
    message_count: i64,
}
```

The subject association is metadata; it is not the thread identity. Multiple
threads may eventually exist for one track or pattern without sharing Python
state.

The full structured message history must be durable, including:

- user and assistant text;
- reasoning parts when retained by the product;
- tool calls and their complete inputs;
- tool results and errors;
- references to generated artifacts.

Persisted transcripts are immutable parent-linked DAGs, not mutable arrays.
A user prompt is durable before its model call begins; a finalized assistant
message is appended afterward. Neither may be edited, reordered, or deleted.
Every append supplies the expected transcript head, inserts a contiguous
message chain plus an immutable append receipt, and advances the local head by
compare-and-swap in one transaction. Redo appends a new user turn. A
conversation rewind creates a new thread whose initial head is an existing
message node; it never rewrites the original thread or any shared message.

The Python workspace is owned one-to-one by `AgentThreadId`, but a thread is
also pinned to the server-observed account principal at creation. A signed-in
user may access only rows bearing that user ID; legacy or newly created `NULL`
rows belong only to the signed-out local principal. The client never supplies
or overrides this owner. Account changes therefore resolve a different thread
and can never reopen another principal's transcript, artifacts, or live Python
namespace.

For a track thread, the pinned venue and persistence score are also part of
authorization scope. Changing the principal, venue, or score resolves a
different thread rather than silently retargeting an existing kernel.

### 5.2 Implemented foundation

The durable foundation now consists of:

- SQLite-backed thread lifecycle rows, immutable parent-linked message rows,
  transcript-head CAS, and append receipts;
- complete AI SDK message parts, including tool calls/results, persisted without
  reducing them to assistant/user prose;
- exact thread reuse by account principal, agent kind, subject, venue, and
  score scope;
- transcript forks that share an immutable prefix and then diverge normally;
- row-sync backup of threads, message nodes, append receipts, transcript-head
  projections, turn preparations/outcomes, and terminal deletion receipts;
- the shared chat/session adapter used by both track and graph agents;
- a Python workspace registry keyed only by durable thread ID;
- non-destructive New Conversation and exact-scope conversation history;
- cancellation propagation from the model turn to the active cell.

These are backend invariants, not conventions that rely on frontend cache
behavior. Every create/read/write/delete/execute command derives the
current principal from trusted host state and requires an exact owner match.
Frontend sessions and bridge registrations nevertheless use the same exact
principal, subject, venue, and score key, so an account change cannot reuse an
already-hydrated transcript and one mounted editor cannot execute or apply
against another scope. The frontend principal is only a memory-cache partition;
the backend remains authoritative. A mounted editor may mirror the committed
timeline for immediate UI feedback, but it does not own the thread, candidate,
or Python kernel.

### 5.3 Conversation lifecycle

Starting a new conversation always creates a new durable thread ID and,
therefore, a new Python workspace. It never deletes, truncates, or repurposes
the previous thread. Conversation history lists only
threads in the exact account/agent/subject/venue/score scope. Reopening one
rehydrates its transcript and workspace association but does not silently
restore its authored state.

Before changing the active conversation, the client stops and drains the
current turn, strictly persists its final transcript, and only then activates
the target. Switches are serialized per exact scope and carry a monotonic
intent, so a slow initial lookup or older click cannot win after a newer
selection. Inactive hydrated chats are evicted from frontend memory; their
immutable transcript nodes, authored revisions, and Python scratch remain.

Navigation, editor unmounting, graph edits, authored-track edits, preview-track
changes, and binding changes do **not** reset a workspace.

### 5.4 One relational revision system for authored state

Authored scores and pattern graphs use the same immutable relational revision
DAG in the app SQLite database. There is no embedded Git repository, ref,
index, checkout metadata, projection ledger, or second filesystem authority.
SQLite owns the canonical bytes, ancestry, current head, validated live
projection, operation outcome, and sync enqueue in one transaction.

The core schema is deliberately small:

| Table | Mutability | Purpose |
|---|---|---|
| `authored_documents` | identity immutable; `archived_at` may transition once | Principal-bound score/graph identity and terminal lifecycle. |
| `authored_revisions` | immutable, permanent | Content-addressed revision metadata and declared parent count. |
| `authored_revision_files` | immutable, permanent | Exact canonical bytes and per-file hash for each path. |
| `authored_revision_parents` | immutable, permanent | Zero, one, or two ordered parent edges; parent 0 is current/ours and parent 1 is merged/theirs. |
| `authored_document_heads` | CAS only | The sole local current-state pointer plus a monotonically increasing generation. |
| `authored_operation_outcomes` | immutable, permanent | Idempotent committed or typed-conflicted result for a host operation. |

The executable SQLite DDL lives in the additive
`src-tauri/migrations/20260802945000_relational_authored_history.sql` migration.
Earlier migrations are frozen byte-for-byte so databases created by the
checkpoint pass SQLx checksum verification; the additive migration removes
their retired Git-shaped tables after establishing the relational replacement.
`20260802950000_agent_trace_remote_hydration.sql` adds only the trusted-pull
admission needed to hydrate immutable traces after terminal lifecycle changes.

One score revision contains exactly `score.luma`. One pattern-graph revision
contains exactly `graph.json` and `layout.json`. Stable database IDs, never
display names, identify documents and entities. Audio, stems, derived analysis,
graph-run output, Python scratch, database timestamps, sync cursors, and
credentials never enter an authored revision.

Each file row stores the exact bytes and a domain-separated SHA-256. The
revision content hash is a domain-separated manifest hash over ordered
`(path, bytes)` pairs. The revision ID is itself a domain-separated hash of the
document ID, ordered parent IDs, manifest hash, and immutable revision metadata.
The server independently recomputes all three before allowing a revision to
participate in a head proposal. Paths are relative, bounded, traversal-free,
and restricted to the exact file set for the document kind.

Revision ancestry is obtained by walking `authored_revision_parents`.
Ancestor checks, first-parent history, and merge-base discovery are relational
queries/helpers over this DAG. Multiple best merge bases are exposed rather
than guessed for strict agent merges; sync has a total fallback described in
§5.10.

### 5.5 Canonical file contracts

Every committed score begins with the exact format envelope
`# luma-score-schema: 1`. It is not an optional comment and is not retained as
score trivia. Canonical serialization always emits the current version;
historical decoding rejects missing, malformed, zero, or unknown versions
before parsing the body. A breaking score grammar or semantic change must bump
this version and retain an explicit decoder/migration for every older version
that can exist in the revision log. Human/model ingress remains free to omit
the envelope: validation resolves that richer draft grammar and writes a
current canonical file at the revision boundary. Ingress also recognizes and
removes one valid current envelope from a workspace file before parsing it as
a draft; the format line never accumulates as an authored comment when that
file is canonicalized. The `luma-score-schema` comment namespace is reserved,
so a malformed or unknown envelope fails instead of being treated as trivia.

The two graph files form one versioned canonical document contract. Both carry
the same required integer `schemaVersion`; `graph.json` contains semantic
nodes, edges, and public arguments, while `layout.json` contains positions
keyed by stable node ID. Neither unversioned input nor a mismatched/unknown
version is accepted. Serialization emits only the current version. Decoding is
strict at every fixed object boundary: unknown graph, node, edge, argument,
layout, or layout-entry fields fail with their full document path instead of
being silently discarded. Node parameter keys and argument values remain
typed payloads rather than fixed document fields.

```text
graph.json  = { schemaVersion, nodes[], edges[], args[] }
layout.json = { schemaVersion, nodes: { <node-id>: { positionX, positionY } } }
```

Historical decoding has two ordered phases: strict decode of the named schema
version, then sequential migrations (`N -> N+1`) until the current structural
model. Any breaking node type, port, or graph-field change must bump the
document version and add that exact migration step. Structural
canonicalization checks bounded shape, identities, endpoints, layout, and DAG
invariants without consulting the installed node catalog, so old revisions and
typed merge bases remain readable. Current-runtime validation is a separate
gate against the installed node types, parameters, ports, and port types. It is
mandatory for workspace ingress and again inside the atomic authored write
before the document head advances; a structurally readable historical graph is
not thereby executable on the current runtime.

### 5.6 Atomic local writes

`authored_document_heads` is the only current-state authority. The ordinary
score/graph tables are validated live projections consumed by the editor and
compositor; they are neither history nor an independently writable copy.
Human UI CRUD, DSL import, undo/redo, Python `track.apply()`, graph saves,
completed turns, restores, and subagent merges all enter through
`AuthoredDocuments`. Low-level projection functions are internal to that
service.

One per-document lock avoids wasted local merge work, but correctness comes
from the database transaction and head CAS. A normal write is:

```text
decode and validate one complete candidate
  -> BEGIN IMMEDIATE
       re-read (head_revision_id, generation)
       verify operation-id replay or collision
       insert immutable revision + files + ordered parents
       update the validated score/graph projection
       UPDATE authored_document_heads
         WHERE revision_id = expected_revision
           AND generation = expected_generation
       insert immutable operation outcome
       enqueue immutable revision closure + head proposal for row sync
     COMMIT
```

The head update must affect exactly one row and increment generation exactly
once. Initial document creation inserts the root revision, projection, and head
in the same transaction. Any failure rolls the whole transaction back; there
is no interval in which history and the live projection disagree, so there is
no projection ledger, publish-after-commit step, startup reconciliation, or
corruption-recovery state machine.

There is one bounded schema-upgrade seed, not an ongoing second-authority
reconciler. After write admission is armed and before sync may run,
`AuthoredDocuments` enumerates every live score and graph projection owned by
the admitted principal that has no `authored_documents` route. It serializes
that projection through the real score/graph codec, creates a deterministic
root revision, head, immutable sync closure, and proposal in one transaction,
then uses route presence as the permanent idempotency marker. The same scan
runs after pull for legacy catalog materialization and when an identity becomes
active. Signed-out rows remain signed-out; signed-in rows are imported only
under their exact owner. A codec/import failure blocks sync for that principal
and remains retryable—it may never silently omit or partially snapshot a
document. Once every legacy projection is seeded, steady-state writes use only
the atomic path above.

Every operation ID is bound to a request fingerprint. An exact retry returns
its immutable outcome; reuse with different input fails. A successful local
head advance also creates one immutable `authored_head_proposals` row. Sync may
later supersede that tip, but it can never erase the revision or its outcome.

The existing row-sync engine uploads document identity, revision metadata, file
bytes, parent edges, and operation outcomes. It registers and pulls proposal,
integration, and archive traces too, but their server rows are created only by
the three RPCs in §5.11. Sync excludes the live authored payloads in
`track_scores` and `implementations.graph_json`, because those are reconstructed
from the pulled head revision. Catalog/routing identity may sync normally only
when it does not become a second payload authority.

### 5.7 Agent-turn transaction

A model turn uses a two-phase boundary because its assistant transcript and
authored state must survive together:

1. `prepare_turn` creates an immutable one-parent revision containing the exact
   proposed state and inserts `authored_turn_preparations` before the assistant
   message is persisted.
2. The complete structured assistant transcript node is appended durably.
3. `finalize_turn` typed-merges that exact prepared revision with the current
   document head, then inserts `authored_turn_outcomes` in the same transaction
   as any revision, projection, head CAS, and sync enqueue.
4. `recover_turns` finds preparations whose assistant node exists but whose
   outcome does not, and finalizes each exactly once after a crash.

Preparation and finalization are keyed by `(thread_id, assistant_message_id)`.
Retries return the original immutable preparation/outcome without re-capturing
newer state. If the live head advanced concurrently, score and graph values are
merged structurally instead of overwriting the newer document. A structured
merge conflict is a durable terminal outcome for that turn: the prepared
revision remains available, the live head is untouched, recovery does not apply
it later, and the conflict is returned as typed data to the orchestrator/agent.
The conversation may continue with another turn.

### 5.8 History, checkpoints, and restore

Every finalized assistant turn names its exact authored revision. State
history contains the current first-parent lineage **and every integrated
proposal tip that was superseded by convergence**. Entries identify whether a
revision is current, an ancestor, or superseded and include its server proposal
sequence when known. Page size is bounded, total history is not. Superseded
tips are visible and restorable; this is an acceptance criterion, not optional
diagnostic data.

Restore never moves a pointer backward or mutates an old revision. It validates
the selected canonical bytes, creates a new one-parent revision whose parent is
the current head and whose bytes equal the selected state, projects it, and
advances the head by CAS. The restore itself is therefore a new forward event.

The restore dialog has exactly two modes:

- **Restore state only:** perform the forward restore above and leave the
  conversation untouched.
- **Restore state and rewind conversation:** perform the same forward state
  restore and atomically create a new thread whose transcript head points to
  the assistant message recorded by that revision's checkpoint. The new thread
  shares the immutable prefix and diverges from there. The original thread
  remains intact and complete; no message row is edited or deleted.

The state-and-conversation option is available only when the selected revision
records a checkpoint for the active thread. The operation is idempotent: its
operation ID deterministically identifies the fork thread and replay returns
the same state revision and fork ID.

### 5.9 Plain isolated subagent workspaces

The authored-state service can materialize the canonical files from an explicit
base revision into a disposable plain directory:

```text
<app-config>/authored-workspaces/<document-id>/<workspace-id>/
  score.luma
  # or graph.json + layout.json
```

`authored_subagent_workspaces` records the workspace ID, owning orchestrator
thread, immutable base revision, current private head revision, generation, and
active/retired status. The directory is only an editing surface. It contains no
repository metadata and is never read as history or authority.

The host exposes one composable contract:

1. **create** — bind an idempotent request to an explicit base revision,
   materialize its bounded canonical file set, and return the directory;
2. **check** — snapshot exactly those files, reject traversal/symlinks/extra
   paths and size violations, decode and validate the complete candidate, and
   return a stable snapshot hash;
3. **commit** — require the expected workspace head and checked snapshot hash,
   canonicalize once, create a one-parent relational revision, CAS the private
   workspace head, and atomically replace the directory with those canonical
   bytes;
4. **merge** — require a clean directory and the expected private tip, merge
   `(recorded base, current live head, private tip)` with `authored_merge`, and
   either advance the live document automatically or store/return typed
   conflicts;
5. **retire** — mark the workspace terminal and remove only the disposable
   directory. Its revisions remain permanent.

A clean subagent result lands on the live document automatically. The
orchestrator is involved only when the semantic merge returns conflicts. Score
clips merge by stable clip ID, graph nodes by stable node ID, edges by target
input slot, and public arguments by stable argument ID. Score trivia/comments
merge through the lossless codec; graph layout never blocks semantic graph
integration. Add/add-different, delete/modify, and divergent scalar changes
produce `AuthoredMergeConflict` values. Text conflict markers are never written
to canonical files or live state. A merge result must pass the authoritative
validator before the service can create its two-parent revision and CAS the
live head.

Multiple children never share a mutable Python namespace or directory. The
supervisor must own child launch, sandbox policy, an immutable process-tree
lease, final snapshot, and full process-tree exit. Check/commit/merge are
independently unit-testable, but production must not hand an untrusted child a
directory until that composed supervisor boundary exists.

### 5.10 Cross-device head convergence

Immutable revisions and their ancestry sync as ordinary append-only rows. A
mutable head does not. Every local head advance submits an immutable proposal
containing `(proposal_id, document_id, device_id, operation_id,
base_revision_id, proposed_revision_id)`. The server assigns a commit-ordered
`server_proposal_seq`; devices never order proposals with client timestamps.

Any online client authenticated as the owner may integrate the earliest pending
proposal, regardless of which device created it. Pulling a pending proposal
enqueues integration locally. Therefore a proposal from a device that goes
offline forever cannot wedge the document. This behavior must be tested with
one device submitting, disappearing, and a different owner device completing
the integration.

For each proposal, an integrator locks/re-reads the current server head and:

1. fast-forwards when the current head is an ancestor of the proposal tip;
2. records `already_ancestor` without moving the head when the proposal tip is
   already contained in current history;
3. otherwise walks the relational DAG for a merge base, combines independent
   changes structurally, and treats the server-ordered proposal as the later
   writer for overlapping fields;
4. validates the whole candidate and uploads any two-parent merge revision;
5. calls the integration RPC with the exact head it computed against. `stale`
   means recompute immediately; `not_earliest` means process the earlier item
   first. Neither is terminal and neither overwrites a head blindly.

The deterministic merge boundary is semantic, not recursive arbitrary JSON:

- scores merge clips by ID, ordinary clip fields independently, and `args` by
  stable argument key; each typed argument payload remains atomic;
- graphs merge nodes by ID and node `params` by stable key, public arguments by
  ID, and each edge atomically by destination node/input slot;
- concurrent presence changes and overlapping scalar values choose the later
  server-ordered proposal.

Sync integration is **total**. After structural composition, the full document
is validated. If composition is invalid—including a graph cycle assembled from
two individually valid branches—the complete later proposal is the terminal
fallback. If the proposal bytes are unreadable or the whole proposal is itself
invalid under authoritative validation, retain the current head pointer and
record `quarantined_noop`. This remains terminal even if the current bytes
cannot be decoded. If merge-base discovery is absent or ambiguous, skip
structural composition and use the same whole-proposal/current fallback. Every
path records an immutable integration result; no proposal remains pending
because semantic resolution was difficult. Authentication, network, and server
availability may retry transport, but no content state can wedge integration.

This is intentionally distinct from agent merging:

| Source of concurrency | Policy | User experience |
|---|---|---|
| Agent turn or subagent workspace | Strict semantic three-way merge | Clean result applies; typed conflicts are stored and returned to the agent/orchestrator. |
| Device synchronization | Server-ordered structural merge plus total deterministic fallback | Always converges silently; no modal, conflict UI, or user question. |

Silent convergence does not delete the losing work. Every proposal tip and
integration row remains permanent. State history includes superseded proposal
tips even when they are not ancestors of the current head, labels them as
superseded, and allows them to be restored through the ordinary forward
restore operation.

### 5.11 Supabase schema, RLS, cursors, and exactly three RPCs

The Postgres surface ships with this design in
`supabase/migrations/20260802000000_authored_revision_sync.sql`; it is not a
future transport. It mirrors these durable rows:

- authored documents, revisions, files, parent edges, operation outcomes;
- server-only document heads, ordered head proposals, integration receipts,
  and terminal archive receipts;
- agent-thread lifecycle rows, immutable transcript nodes, append receipts,
  server-only transcript heads, turn preparations/outcomes, and deletion
  receipts.

Postgres triggers reject deletion of every immutable trace and reject any
update that is not byte-for-byte replay of the same row. They independently
verify principal ownership, revision/file/manifest hashes, exact file shape,
parent closure/order, acyclic ancestry, bounds, and revision identity. Document
identity is immutable and `archived_at` can transition only inside the archive
RPC. Transcript messages validate principal, parent, depth, and assistant-turn
preparation; append receipts validate a contiguous parent-linked range. The
server transcript head is a projection advanced by append receipts in server
commit order; clients never upload a blind transcript-head snapshot. A fork
thread may sync before its prefix node: its head remains empty until that
immutable node arrives, then a trigger installs the shared prefix
deterministically. Concurrent valid appends are both retained; the later
server-committed receipt selects the projected head while the sibling remains
an immutable trace.

RLS permits an authenticated principal to select its own rows and insert or
exact-replay its immutable inputs. The following server projections/outcomes
are select-only to clients: `authored_document_heads`,
`authored_head_proposals`, `authored_head_integrations`,
`authored_document_archives`, and `agent_thread_transcript_heads`. Private hash,
clock, ancestry, closure, and trigger functions stay in the non-exposed
`private` schema. There are exactly three new public RPCs:

Signed-out `principal_key = 'signed-out'` rows remain local; they are not
uploaded or silently rebound to a later account. Signed-in ownership is always
derived from `auth.uid()`/trusted host state, never from a caller-selected key.

1. `submit_authored_head_proposal(proposal_id, document_id, device_id,
   operation_id, base_revision_id, proposed_revision_id, created_at)` verifies
   the complete immutable revision closure, assigns proposal order, and returns
   an idempotent receipt. A proposal arriving after archive is immediately
   terminal as `cancelled_archived`.
2. `integrate_authored_head_proposal(proposal_id,
   expected_head_revision_id, resolution, result_revision_id)` locks the
   document, accepts only the earliest pending proposal, checks the expected
   head and the claimed ancestry/two-parent shape, advances the head, and writes
   one immutable terminal integration receipt. It returns `stale` or
   `not_earliest` without mutation so any online client can recompute.
3. `archive_authored_document(archive_id, document_id, device_id,
   operation_id, requested_revision_id, archived_at)` locks the document,
   performs the one-way archive transition, captures the final head, and
   terminally cancels every pending proposal in server order. Racing archive
   requests each retain an immutable receipt.

No fourth head, merge, transcript, or archive RPC is permitted. Immutable data
uses the existing row-sync engine; mutable authored and transcript heads are
server projections driven by the protocols above.

Every syncable table uses a server-assigned `sync_seq`. Allocation is protected
by one transactional row lock, so sequence N commits before N+1 can be
allocated. Pull is `sync_seq > cursor ORDER BY sync_seq`; the cursor advances
only after local application. Client clocks and `updated_at` are never pull
cursors, eliminating the late-commit hole in the prior timestamp design.

### 5.12 Terminal archive and deletion

Archive is permanent. Once `authored_documents.archived_at` is set, neither
local nor remote sync may clear it, recreate a live head, reinsert a deleted
score/pattern projection, or accept another normal mutation. Pending proposals
become terminal `cancelled_archived`. Revisions, files, parent edges, proposals,
integrations, operation outcomes, and archive receipts remain readable as
history; only live catalog/projection rows may be removed.

Thread deletion is likewise terminal but does not erase trace data. A durable
deletion receipt closes the lifecycle and prevents later row sync from
resurrecting the thread or its mutable transcript-head projection. Immutable
message nodes, append receipts, turn preparations/outcomes, and shared fork
prefixes survive. Local Python scratch and active isolated directories may be
garbage-collected after the terminal receipt is durable.

### 5.13 Retired prototype and migration boundary

The embedded Git design was checkpointed in repository commit `2edab24` before
replacement. That prototype exists only on `agent-code-execution`, is not an
ancestor of `origin/main`, and was never a released storage format. Historical
SQLite migrations nevertheless remain byte-for-byte unchanged: SQLx can open a
database created by the checkpoint, and the additive relational migration
preserves its live score/graph projections plus thread/transcript data before
dropping the retired ledgers. The host then serializes every unopened live
projection through the authoritative codecs into a deterministic relational
root before sync starts. No migrated document depends on being opened by the
user first.

The prototype's bare Git object database is deliberately not a product input.
The app ships no dual reader, libgit2 importer, or permanent compatibility
layer; pre-replacement Git-only intermediate commits do not become relational
revisions. The live canonical state is preserved as the new root, conversation
traces are preserved, and commit `2edab24` is the archaeology path for the
unreleased prototype history. Relational history is lossless and syncable from
that root forward.

The replacement removes, rather than ports:

- the `git2`/libgit2 dependency and Git object/ref wrapper;
- bare-repository and linked-checkout storage paths;
- repository, branch, ref-CAS, and checkout identifiers from public models;
- projection-ledger and startup reconcile machinery;
- Git thread branches, turn trailers, and Git-specific operation recovery;
- the old `authored_state_projections`, thread-branch, turn-commit,
  operation-ledger, and checkout-routing tables;
- Git-specific commands, harness paths, and tests.

They are replaced by the revision DAG, operation/turn outcomes, plain isolated
workspace rows, and row-sync protocol in this section. Do not retain aliases,
dead modules, compatibility shims, or stale checkout vocabulary.

The remaining risks are explicit and narrow: production subagents still need a
composed sandbox/supervisor lease; total sync integration depends on at least
one owner client being online and able to validate the document kind; and
immutable trace growth needs retention/quotas for non-product artifacts, not
history deletion. None justifies a second authored-state authority.

---

## 6. Core invariants

These are architectural requirements.

1. One agent thread owns exactly one logical Python workspace.
2. Workspaces never share a Python namespace.
3. A live kernel is created lazily on the thread's first Python call.
4. Only one cell runs at a time within a workspace.
5. Different workspaces may execute concurrently.
6. Before every cell, the host atomically reinstalls the reserved `luma`
   binding from one immutable binding revision.
7. Agent-created variables remain intact when `luma` is refreshed.
8. Reassigning or deleting `luma` inside a cell affects only that cell; the host
   reinstalls it before the next one.
9. All Luma data enters Python through one binding-manifest contract.
10. All large inputs and outputs use one artifact store.
11. The model never supplies host file paths or binding operations.
12. Every multidimensional numerical value carries semantic axes.
13. Times used for cross-domain comparison have explicit units and time origin.
14. Spatial tensor rows have explicit primitive identities.
15. Missing or failed data is distinguishable from genuinely empty data.
16. Bound application values are immutable. Mutation is possible only through
    an explicit, scope-bound host capability.
17. Every score and graph mutation passes through the canonical relational
    authored-document service; typed score/graph projection functions are
    internal implementation details, never parallel runtime authorities.
18. Worker death or forced termination never masquerades as preserved state.
19. Production execution hard-stops if the sandbox cannot be established.

---

## 7. Agent-facing Python experience

### 7.1 The tool

Both agents receive the same model-facing tool:

```ts
python({
  purpose: string,
  code: string
})
```

`purpose` is a short noun phrase used only to label the running cell in the UI
(for example, `"section energy analysis"`). It does not select
scope, authority, execution policy, or a different operation. `code` is the
ordinary cell-shaped Python source.

For the track copilot this is the only model-facing tool. Track discovery,
analysis, visualization, and authoring all happen through Python over the same
bound values. The graph agent may retain its existing validated graph-mutation
interface; it must not grow a second analysis executor.

The model does not choose:

- workspace or thread IDs;
- binding revisions;
- track, venue, score, pattern, or graph-run IDs;
- input paths;
- artifact paths;
- timeout or sandbox policy.

The agent adapter resolves all of those from the current durable thread and live
editor bridge.

Suggested tool description:

> Execute Python in a namespace persistent for this agent thread. Current
> Luma bindings are available under `luma` and are refreshed before every call.
> Variables, functions, and imports you create persist. The last expression,
> stdout, stderr, exceptions, and figures are returned. Use normal
> Python/NumPy/SciPy/librosa/matplotlib code. In an editable track thread,
> create one staged candidate with `luma.track.edit()`, inspect it, and call
> `apply()` only when it is ready.

The code is normal cell-shaped Python. It does not require a wrapper function or
an explicit `return`.

### 7.2 Notebook semantics

The kernel preloads or makes available:

```python
import numpy as np
import scipy
import scipy.signal
import librosa
import matplotlib.pyplot as plt
```

Definitions persist:

```python
def nearest_error(reference, candidates):
    return np.array([
        np.min(np.abs(candidates - t))
        for t in reference
    ])
```

A later cell may use `nearest_error` without redefining it.

The last expression is displayed automatically:

```python
np.quantile(luma.features.bars.intensity.values, [0.25, 0.5, 0.9])
```

The worker also exposes ordinary `print()` and notebook-style figure display.

### 7.3 The `luma` namespace

The stable top-level shape is:

```python
luma.meta
luma.window
luma.track
luma.audio
luma.features
luma.venue
luma.patterns
luma.graph
```

For a track thread, `luma.track` is a small domain object backed by the ordinary
binding tree. It exposes track metadata plus:

```python
luma.track.revision          # semantic revision of all authored clips
luma.track.editable          # descriptive; the host rechecks authorization
luma.track.clips             # immutable, complete, lossless clip snapshot
luma.track.edit()            # start a staged full-candidate transaction
```

Each clip contains its stable ID, stable pattern ID, optional pattern display
name, exact start and end seconds, explicit `z`, blend mode, and the complete
JSON argument value. Display ordering is time-major for readability; `z`, not
file/list order, defines stack semantics.

Branches not applicable to a given agent or unavailable for the current scope
remain discoverable but report why they are unavailable.

`luma.catalog()` returns a compact description of:

- available paths;
- tensor shapes and dtypes;
- axis names, units, and labels;
- provenance and processor version;
- unavailable paths and reasons.

It is an explicit escape hatch, not a mandatory first call and never forced
into model context. Normal discovery should be incremental: inspect a branch,
use `dir(...)`, read a small repr or slice, then go deeper only where the task
requires it. Large catalogs and arrays must not consume the first turn merely
to prove that data exists.

`luma.meta.revision` and scope information are available for debugging, but
routine tool output does not dump revision bookkeeping at the model.

### 7.4 Audio and musical features are distinct

This distinction is fundamental:

- `luma.audio` contains audio signals: the mix and stems.
- `luma.features` contains information derived from audio: beats, downbeats,
  drum onsets, bar classifications, chords, mel data, waveform bands, and MERT
  features when exposed.

Neither is a fallback for the other. The agent uses the namespace matching the
question.

Use already-derived drum onsets:

```python
kicks = luma.features.drum_onsets["kick"].values
snares = luma.features.drum_onsets["snare"].values

{
    "kick_count": len(kicks),
    "snare_count": len(snares),
    "median_kick_gap_s": float(np.median(np.diff(kicks))),
}
```

Operate on audio:

```python
drums = luma.audio.stems["drums"]

envelope = librosa.onset.onset_strength(
    y=drums.values,
    sr=drums.sample_rate_hz,
)

threshold = np.quantile(envelope, 0.88)
peaks, props = scipy.signal.find_peaks(
    envelope,
    height=threshold,
    prominence=np.std(envelope) * 0.6,
)
```

The first example asks questions about Luma's precomputed onset feature. The
second asks a new question of the audio signal itself.

### 7.5 Correlating audio features and lighting output

```python
view = luma.graph.run.views["view_signal_1"]
dimmer_channel = view.channels.index("dimmer")
dimmer = view.values[:, :, dimmer_channel].mean(axis=0)

light_peaks, _ = scipy.signal.find_peaks(dimmer, prominence=0.2)
light_times = view.times_s[light_peaks]
kick_times = luma.features.drum_onsets["kick"].values

errors = np.array([
    np.min(np.abs(light_times - kick))
    for kick in kick_times
])

{
    "median_lag_ms": float(np.median(errors) * 1000),
    "p90_lag_ms": float(np.quantile(errors, 0.9) * 1000),
}
```

This is the core loop the system must make easy.

### 7.6 Plotting

```python
fig, ax = plt.subplots(figsize=(12, 4))
ax.plot(view.times_s, dimmer, label="mean dimmer")
ax.vlines(
    kick_times,
    ymin=0,
    ymax=1,
    alpha=0.25,
    label="detected kicks",
)
ax.set_xlabel("absolute track time (s)")
ax.legend()
fig
```

The model receives the figure as an image part in the tool result. The host also
registers the PNG under the workspace's artifact store, but the current
transcript representation does **not** retain that artifact reference: it keeps
base64 strings up to 2,000,000 characters and stores only a placeholder for a
larger figure. Persisting only an artifact ID and resolving it for transcript
replay is remaining work. Until that exists, figures are a deliberate bounded
exception to artifact-only transcript persistence; large numerical tensors are
not.

### 7.7 Mutation boundary

Python sees immutable snapshots of the current graph, graph output, venue,
track, clips, audio, and analysis products. It receives no database handle and
no generic mutation callback.

Track authoring is the single deliberate exception. `luma.track.edit()` creates
a Python-local mutable object containing the **entire** clip candidate and its
optimistic base revision. Only that object has the three domain mutations:

```python
edit.add_clip(...)
edit.update_clip(...)
edit.remove_clip(...)
```

Those methods do not touch live state. `edit.diff()` and local checks are also
non-mutating. `edit.check()`, candidate rendering, and `edit.apply()` cross the
sandbox through a narrow named host-call protocol. A coherent, exact
score/track/venue scope is enough to inspect the committed timeline and render
its compositor heatmap; creating an edit, checking it, or applying it also
requires authenticated score-owner authority. The trusted host owns scope,
authorization, pattern compilation, compositing, validation, ID assignment,
and the database transaction. The protocol is an internal implementation
detail, not another model tool.

Graph mutations remain behind the graph agent's canonical validated interface.
They are intentionally independent of the track transaction described here.

After a successful track apply or graph mutation, the next Python cell receives
a refreshed `luma` binding while agent-created variables remain. A successful
`edit.apply()` closes that edit; further changes begin from a freshly bound
track and revision.

The track loop is:

```text
inspect and compute with Python
    -> stage one complete candidate
    -> inspect timeline and composited output
    -> diff and check
    -> atomically apply
    -> inspect the refreshed track with Python
```

---

## 8. Semantic tensors: the core data model

Luma's evaluator is already fundamentally tensor-shaped: values flow between
nodes and produce spatial-temporal lighting signals. The agent data plane must
preserve that model rather than reducing it to anonymous arrays.

### 8.1 Python representation

Numerical bindings are exposed through a lightweight `LumaTensor` envelope:

```python
tensor.values       # lazy, read-only numpy.ndarray
tensor.axes         # ordered semantic axes
tensor.unit         # optional unit for values
tensor.provenance   # source, processor version, and relevant metadata
tensor.shape
tensor.dtype
```

`LumaTensor` supports `np.asarray(tensor)` and can provide convenience
properties derived from axes:

```python
tensor.times_s
tensor.primitive_ids
tensor.channels
tensor.frequencies_hz
```

It is not a new numerical computing library. NumPy remains the computation
engine; `LumaTensor` preserves meaning at the boundary.

### 8.2 Axis model

Every tensor axis has:

```rust
enum AxisSpec {
    Linear {
        name: String,
        start: f64,
        step: f64,
        count: usize,
        unit: Option<String>,
    },
    Coordinates {
        name: String,
        tensor: TensorRef,
        unit: Option<String>,
    },
    Labels {
        name: String,
        labels: Vec<String>,
    },
    Index {
        name: String,
        count: usize,
    },
}
```

Examples:

- evenly sampled audio: linear `time` axis;
- graph view: exact or linear absolute `time` axis;
- onsets: `event` index with timestamp values in seconds;
- venue positions: labeled `primitive` and `coordinate=["x","y","z"]`;
- graph signal: labeled `primitive`, absolute `time`, labeled `channel`;
- mel spectrogram: frequency and time coordinates;
- MERT: frame/time and feature axes;
- bar predictions: bar coordinates and tag labels.

### 8.3 Canonical numerical shapes

| Binding | Tensor shape | Required semantics |
|---|---|---|
| Audio mix | `[time]` or `[time, channel]` | absolute time, sample rate, channel labels |
| Audio stem | `[time]` | absolute time, sample rate, stem name |
| Beats/downbeats | `[event]` | values are absolute seconds |
| Drum onsets | `[event]` per class | values are absolute seconds, class provenance |
| Bar intensity | `[bar]` | bar index plus start/end seconds |
| Bar predictions | `[bar, tag]` | tag labels and processor version |
| Chord roots | `[section]` | section start/end, pitch class or missing |
| Waveform bands | `[band, time]` | band labels and bucket times |
| Mel spectrogram | `[frequency, time]` | frequency/time coordinates |
| MERT | `[frame, feature]` | frame times and model/layer provenance |
| Venue positions | `[primitive, coordinate]` | primitive IDs and xyz labels |
| Fixture attributes | `[primitive, attribute]` | primitive IDs and attribute labels |
| Graph view | `[primitive, time, channel]` | labeled primitive IDs when the tap is actually primitive-indexed; an explicit index axis for broadcast/mismatched taps; absolute time and channel meaning |
| Track candidate output | `[light, time, RGB]` | stable light IDs, exact absolute time, `r/g/b`; color already multiplied by dimmer |

### 8.4 Time semantics

All cross-domain times are absolute track seconds unless a binding explicitly
declares another origin. Graph-agent audio may be span-sliced for efficiency,
but its time axis remains absolute.

The analysis window is separately available at:

```python
luma.window.start_s
luma.window.end_s
```

No agent code should have to reconstruct graph times with `linspace(span)`.

### 8.5 Spatial identity

Any tensor genuinely indexed by spatial `primitive` or `light` identity must
attach the exact ordered IDs used by its evaluator/compositor. Positions,
attributes, graph-view rows, and candidate-output rows must share compatible
identities where they are combined or fail binding validation.

A graph tap may instead be broadcast (`n == 1`) or otherwise disagree with the
run's primitive count. The current provider publishes that dimension as an
unlabeled `Index("primitive")` axis rather than inventing fixture identity. Such
a tensor is valid for inspecting the signal, but it must not be joined to venue
positions or treated as one row per fixture until it has been explicitly
broadcast against a labeled primitive axis.

Shape equality without identity equality is insufficient.

---

## 9. One binding and artifact data plane

### 9.1 Manifest

All host data is described by one recursive manifest:

```rust
struct BindingManifest {
    schema_version: u32,
    revision: BindingRevision,
    scope: AnalysisScope,
    root: BindingValue,
    artifacts: BTreeMap<ArtifactId, ArtifactDescriptor>,
}

enum BindingValue {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    List(Vec<BindingValue>),
    Record(BTreeMap<String, BindingValue>),
    Tensor(TensorRef),
    Unavailable {
        reason: String,
        provenance: Option<Provenance>,
    },
}

struct TensorRef {
    artifact_id: ArtifactId,
    encoding: TensorEncoding,
    dtype: DType,
    shape: Vec<usize>,
    byte_offset: u64,
    axes: Vec<AxisSpec>,
    unit: Option<String>,
    read_only: bool,
    provenance: Provenance,
}
```

The manifest is an internal host/worker contract. It is not dumped into model
context.

### 9.2 Binding providers

Domain modules contribute through one builder API:

```rust
builder.inline("track", track_metadata)?;
builder.tensor("features.beats", beat_tensor)?;
builder.tensor("venue.positions", positions_tensor)?;
builder.record("track.clips", authored_clips)?;
builder.unavailable("audio.stems", "stem preprocessing has not completed")?;
```

Expected providers:

```text
agent_bindings/
  track
  audio
  features
  venue
  patterns
  graph_definition
  graph_run
```

Providers understand their domain sources but know nothing about Python.
The Python loader understands the manifest but knows nothing about SQLite,
Tauri, graph compilation, or venue databases.

This is one system even though several domain providers contribute to it.

### 9.3 Artifact store

Large values live in one artifact store and are referenced by opaque IDs.

An artifact descriptor contains host-internal resolution information:

```rust
struct ArtifactDescriptor {
    id: ArtifactId,
    kind: ArtifactKind,
    encoding: String,
    byte_len: u64,
    content_hash: Option<String>,
    ownership: ArtifactOwnership,
}
```

The model never receives or chooses the host path.

Supported initial codecs:

- `raw_le`: headerless contiguous little-endian numerical data;
- `npy`: existing or newly written NumPy arrays;
- `pcm_f32`: existing Luma PCM cache with header/offset metadata;
- `png`: figures;
- bounded UTF-8 or JSON artifacts for explicitly exported proposals.

One mechanism does **not** mean one physical encoding. It means:

- one manifest;
- one artifact identity system;
- one loader;
- one permission policy;
- one lifecycle;
- one agent-facing namespace.

### 9.4 Reusing existing files

Existing caches should be imported without needless conversion:

- `.pcm` audio uses its existing 18-byte header:
  `version u32 LE | sample_rate u32 | channels u16 | len u64`, then `f32 LE`;
- MERT's `.fullmix.npy` and `.drum.npy` remain `.npy`;
- fresh Rust `Vec<f32>` values such as graph views can be materialized as
  headerless raw files plus complete tensor metadata;
- small values remain inline.

The Python materializer returns a uniform `LumaTensor` regardless of codec.

### 9.5 Workspace-visible inputs

Each workspace has an app-owned directory:

```text
agent-workspaces/<thread-id>/
  inputs/       read-only inside the sandbox
  scratch/      writable inside the sandbox
  outputs/      host-registered generated artifacts
```

The artifact store makes the current revision's inputs available under
`inputs/`, using hard links, reflinks, or copies as appropriate. The worker does
not receive arbitrary original music-library paths.

This allows a stable sandbox policy: read the workspace's input root and write
only its scratch/output roots.

### 9.6 Immutability and revisions

Binding revisions are immutable. Each cell pins exactly one revision for its
duration.

When app state changes, including after a successful track transaction:

1. providers assemble a new revision;
2. the next cell receives the new `luma`;
3. agent-created variables remain;
4. old artifacts remain leased while a cell or retained artifact references
   them;
5. unreferenced transient revisions can be collected.

The host tracks revisions for correctness. Routine model output does not include
revision IDs.

### 9.7 Unavailable versus empty

These must remain different:

```python
luma.features.drum_onsets["kick"]  # available tensor with length 0
```

versus:

```text
Unavailable("drum-onset preprocessing failed: …")
```

Providers must preserve source errors instead of converting every failure to
`None`, `{}`, or `[]`.

---

## 10. Concrete `luma` binding schema

### 10.1 Shared

```text
luma.meta
  schema_version
  revision                  debugging only
  agent_kind
  availability/catalog

luma.window
  start_s
  end_s

luma.track
  id
  title
  artist
  album
  duration_s
  bpm
  key
  revision                  semantic hash of the complete authored clip set
  editable                  authenticated owner may create/check/apply an edit
  clips                     immutable complete clip snapshot
    id                      stable persisted ID
    pattern_id              stable identity; display names are never authoritative
    pattern_name            optional display convenience
    start_s                 exact absolute track seconds
    end_s                   exact absolute track seconds
    z                       explicit stack order, including negative/sparse values
    blend                   blend mode
    args                    complete lossless JSON value

luma.audio
  mix                       lazy AudioTensor
  stems
    drums                   lazy AudioTensor
    bass
    vocals
    other

luma.features
  beats                     event-time tensor
  downbeats                 event-time tensor
  bpm
  beats_per_bar
  drum_onsets
    kick                    event-time tensor
    snare
    hat
    cymbal
  bars
    indices
    starts_s
    ends_s
    intensity
    predictions             [bar, tag]
    tags
  chords
    starts_s
    ends_s
    root_pitch_class
  waveform_bands            [band, time]
  mel                       [frequency, time]
  mert                      optional [frame, feature] products

luma.venue
  id
  name
  fixtures
  pieces                    set design; flattened world pose per piece
  groups
  positions                 [primitive, xyz]
  uv                        [primitive, uv] rig-intrinsic pattern space
  views                     camera names render(view=...) accepts
```

### 10.2 Track agent

```text
luma.patterns
  summaries
  argument_schemas
```

The authored lighting timeline does not live in a parallel namespace branch.
It lives directly on `luma.track` because it is the thing being understood and
edited.
“Score” remains useful persistence vocabulary (`score_id`, `track_scores`) at
the Rust/SQLite boundary, but it is not an additional agent concept.

The Python loader materializes the bound track record as the `Track` facade.
That facade preserves ordinary metadata access and adds `edit()` and `window()`;
it does not create a second data source.

The graph branch is unavailable unless a graph run is deliberately placed in
the track thread's scope.

### 10.3 Graph agent

```text
luma.graph
  definition
    nodes
    edges
    args
    arg_values
  run
    views                    name -> semantic tensor
    mel_views
    primitive_ids
    positions
    span
    fingerprint/provenance
```

The graph agent does not receive the whole authored timeline by default. Its
subject is one pattern and one preview instance.

### 10.4 Laziness

All artifact-backed tensors may materialize lazily. From the agent's
perspective, access remains ordinary:

```python
luma.audio.mix.values
luma.graph.run.views["view_signal_1"].values
```

The Python loader caches an opened artifact by immutable artifact ID. It uses
read-only mappings where beneficial and ordinary reads where mapping is slower.
The model does not choose the loading strategy.

---

## 11. Graph-run integration

### 11.1 Current problem

`run_graph` currently:

- builds a `ResidentContext`;
- compiles the graph;
- evaluates exact preview times;
- owns the ordered primitive IDs;
- returns `Signal {n,t,c,data}` views;
- returns mel specs only for graph `mel_spec_viewer` nodes;
- moves the compiled plan into the live scene.

The returned `RunResult` omits:

- exact time coordinates;
- primitive IDs for signal rows;
- channel semantics;
- graph/argument/scope fingerprints.

The agent keeps the last result only in a frontend ref. There is also no
structural guarantee that a cached result still matches a subsequently edited
graph.

### 11.2 Required refactor

Extract graph evaluation into an internal result:

```rust
struct GraphEvaluation {
    views: HashMap<String, SemanticSignal>,
    mel_views: HashMap<String, SemanticMel>,
    times_s: Vec<f32>,
    primitive_ids: Vec<String>,
    positions: Vec<[f32; 3]>,
    span: (f32, f32),
    graph_hash: String,
    arg_hash: String,
    selection_hash: String,
    track_id: String,
    venue_id: String,
}
```

Two consumers derive from it:

1. the existing UI-facing `RunResult`;
2. the generic graph-run binding provider.

When the run is associated with an agent thread, the provider publishes it
under `luma.graph.run` through the normal binding/artifact system before Rust
drops or moves the evaluation buffers.

The association may be passed to the command as a separate optional
`AgentThreadId`/publish target. It does not belong inside the semantic
`GraphContext` model.

### 11.3 Compatibility

A graph-run contribution is usable only when these match the current scope:

- track ID;
- venue ID;
- span;
- graph hash;
- argument values;
- preview selection/instance seed when they affect output.

Otherwise `luma.graph.run` reports that the graph has changed since its latest
run. It must never silently pair a new audio/track scope with an old graph
tensor.

---

## 12. Source-data integration

### 12.1 Do not teach the executor cache paths

The executor must not reconstruct domain file paths. Domain services resolve
beats, stems, audio, roots, MERT, waveforms, venue data, and scores.

In particular, `eval/context.rs` currently reconstructs audio/stem cache paths
using `HOME` and macOS-specific `Library/Application Support` paths. Those
resolvers should be centralized through `AppHandle`/existing storage helpers
before being reused by the binding providers.

### 12.2 Reuse, but do not directly reuse, `ResidentContext`

`ResidentContext` already contains much of the graph-side data:

- positions;
- beat grid;
- audio;
- stems;
- attributes;
- drum onsets;
- chords;
- span.

However, `build_resident_context` intentionally loads only data consumed by the
current graph. The open-ended agent bindings must expose available data even
when the graph does not currently reference it.

Extract shared domain-loading helpers or repositories and let both:

- evaluator context construction; and
- agent binding providers

consume those helpers.

Do not make the executor depend on evaluator-only loading conditions.

### 12.3 Known source caveats

- `MelSpec` currently has width, height, data, and an optional beat grid but no
  explicit frequency/time coordinates. The provider must add them.
- Waveform bands have values but no explicit bucket time axis. Derive it from
  decoded duration and bucket count.
- `ResidentContext.attributes` is declared but is not currently populated in
  `build_resident_context`; do not advertise a broad attribute tensor until the
  data is real.
- Unsaved graph definition state is frontend-owned. The trusted bridge may
  contribute small inline graph-definition bindings, while graph-run arrays are
  published in Rust.
- Pattern summaries and argument schemas may likewise be contributed by a
  trusted app adapter when their canonical current copy is frontend-owned.

All contributions still use the same manifest type.

---

## 13. Workspace and kernel lifecycle

### 13.1 Minimal ownership model

The required registry is intentionally small:

```rust
struct PythonWorkspaceService {
    workspaces: HashMap<AgentThreadId, WorkspaceHandle>,
}
```

A workspace is created lazily. It is not tied to a mounted React component.

### 13.2 Live persistence

While the kernel remains alive, variables survive:

- multiple Python calls;
- multiple model turns;
- navigation away from the editor;
- returning to the same thread;
- changes to the current `luma` binding revision.

### 13.3 Durable workspace data

The current implementation persists:

- complete executed cell source through structured tool history;
- bounded cell outcomes in structured tool history;
- the workspace directory, including explicit scratch files and generated
  input/output bytes.

The in-memory artifact registry, leases, and kernel namespace do **not** restore
after an app restart. Reopening an existing workspace currently creates an
empty artifact registry even though old files may remain on disk, so those bytes
are not discoverable by their old artifact IDs. Durable artifact metadata,
transcript artifact references, and safe reconciliation/collection of files
already on disk are remaining hardening work.

Do not duplicate cell source into an unrelated second history system when the
structured thread already contains it.

### 13.4 App restart

An arbitrary live CPython namespace cannot be reliably serialized. Modules,
native objects, open files, iterators, closures, memmaps, background threads,
and C-extension state make generic pickle/dill persistence incomplete and
fragile.

The current semantics are:

- structured cells and workspace files remain beside the durable thread;
- a live kernel and its arbitrary namespace exist only while that process is
  alive;
- after app restart, a fresh kernel starts with no restored variables, imports,
  functions, open files, or artifact registry;
- a worker death detected by the still-running host produces a concise
  state-loss notice, but a full app restart is not yet detected as prior-state
  loss and therefore does not currently emit that notice.

Persist enough workspace metadata to distinguish a thread's first-ever kernel
from a post-restart kernel, then surface the same concise state-loss notice on
the first cell after restart. That is required hardening, not an implemented
guarantee today.

Best-effort cell replay may be added later, but it is not exact restoration and
must not be silently represented as such. True exact restoration would require
process/VM checkpointing and is outside this design.

### 13.5 New conversation and deletion

- New conversation: create a new thread and Python workspace association.
  Never clear or reuse the old identity. An authored subagent workspace is
  created only for an isolated child job, not for an ordinary conversation.
- Explicit Python reset: replace the process, not merely `globals().clear()`.
- Thread deletion: terminate the kernel and remove thread-owned scratch and
  unreferenced artifacts, retire every child authored workspace, record the
  terminal deletion receipt, and remove the lifecycle/head routing rows.
  Immutable transcript nodes and authored revisions remain, so deletion cannot
  erase trace history or a prefix shared by another thread.
- Thread archive/navigation: may stop the live kernel later if a trustworthy
  restoration policy exists; no idle eviction is required initially.

Replacing the process on an explicit Python reset is important because code can mutate
module globals, matplotlib configuration, native-library state, and background
threads outside the user globals dict.

---

## 14. Worker architecture

### 14.1 Subprocess, not embedded Python

Run CPython as a subprocess.

Reasons:

- crash isolation from native extensions;
- an OS sandbox can wrap the whole process;
- process-group interruption and forced termination are available;
- no Python ABI/PyO3 packaging coupling;
- existing bundled Python and managed venv are reusable;
- subinterpreters are unsuitable for NumPy-heavy execution.

The existing one-shot Python workers prove interpreter provisioning and
dependency installation, not this persistent runtime. The new executor still
requires a worker protocol and lifecycle layer.

### 14.2 Python worker responsibilities

`luma_exec/worker.py` owns:

- persistent user namespace;
- installation of the current `luma` binding;
- manifest and artifact materialization;
- preloaded analysis libraries;
- notebook last-expression evaluation;
- stdout/stderr capture;
- traceback formatting;
- matplotlib figure capture;
- bounded representations;
- the generic synchronous host-call transport;
- one request loop.

It knows nothing about tracks, patterns, scores, venues, Tauri, or SQLite.

### 14.3 Host process responsibilities

Rust owns:

- mapping thread IDs to workers;
- constructing binding revisions;
- artifact resolution and leases;
- sandboxed worker launch;
- request correlation;
- timeouts and cancellation;
- process-group signals;
- output and artifact quotas;
- crash detection and state-loss notification;
- installing an optional scoped, allowlisted host-call handler for a cell.

### 14.4 Internal wire protocol

Use newline-delimited JSON over pipes. This is an internal worker protocol, not
the model-facing tool result.

Conceptual request:

```json
{
  "id": "cell-17",
  "op": "exec",
  "code": "kicks = luma.features.drum_onsets['kick'].values\nlen(kicks)",
  "manifest_rel": "inputs/manifest-r-x.json"
}
```

The host owns timeout policy; it is not a worker- or model-selected request
field.

Conceptual frames:

```json
{"id":"cell-17","type":"started"}
{"id":"cell-17","type":"stream","stream":"stdout","text":"..."}
{"id":"cell-17","type":"stream","stream":"stderr","text":"..."}
{"id":"cell-17","type":"result","status":"ok","repr":"128","artifacts":[]}
```

`started` is a synchronization boundary, not progress decoration. The worker
emits it only after installing the requested binding revision and parsing the
cell, once its execution guard is active and immediately before entering user
code. Rust correlates it to the execution ID and withholds a cancellation
`SIGINT` until that acknowledgement. Without the handshake, a signal sent after
the request was written but before Python entered the guarded region could be
correctly ignored as a between-cell signal, after which the cancelled cell
would run indefinitely.

During an execution, a bound domain facade may make a synchronous internal host
call:

```json
{"id":"cell-17","type":"host_call","call_id":"h-1","method":"track.check","payload":{"baseRevision":"…","candidate":[]}}
{"id":"cell-17","op":"host_response","call_id":"h-1","ok":true,"value":{"baseRevision":"…","candidate":[]}}
```

Calls are correlated to the active cell, bounded in count, and allowed only from
the cell's main thread. Rust answers each call through the handler installed for
that cell. Missing handlers and unknown methods return structured errors. The
transport is deliberately domain-free: it carries JSON method/payload/result
frames but owns no track policy.

Exactly one terminal result finishes a request. `started` is non-terminal and
must occur at most once for an execution. Interrupt is out-of-band via an OS
signal, never queued as an NDJSON command behind running code.

NDJSON was measured at tens of microseconds per trivial round trip. Jupyter,
ZeroMQ, Arrow RPC, or a socket protocol do not solve a problem present here.

### 14.5 Protocol stdout isolation

Agent `print()` and native libraries can write directly to file descriptor 1.
If protocol frames also use fd 1, those bytes can corrupt framing.

At worker startup:

```python
proto_fd = os.dup(1)
capture_r, capture_w = os.pipe()
os.dup2(capture_w, 1)
proto = os.fdopen(proto_fd, "w", buffering=1)
```

The private duplicate carries protocol frames. The redirected fd captures both
Python and native stdout. `contextlib.redirect_stdout` alone is insufficient.

Stderr needs equivalent deliberate treatment or a separate host-consumed pipe.

### 14.6 Displayhook

Parse the cell with `ast`. If its final statement is an expression:

1. execute the preceding statements;
2. evaluate the final expression;
3. render a bounded notebook-style representation.

Do not require `return`; models naturally write notebook-shaped Python.

Catch `BaseException`, not only `Exception`, so `KeyboardInterrupt` becomes a
result rather than killing the worker read loop.

### 14.7 Figures

Set:

```python
matplotlib.use("Agg")
```

before importing pyplot.

After a cell:

1. discover newly created figures;
2. save them into the thread output area;
3. close them;
4. register them with the artifact store;
5. return workspace-relative figure references to the host;
6. read bounded PNG bytes into the model-facing image parts.

Today the frontend transcript retains bounded base64 rather than the registered
artifact ID, as described in §7.6. The intended follow-up is to persist artifact
references and create base64 only transiently when the AI SDK/provider needs an
image block.

### 14.8 Output limits

Bound:

- last-expression representation;
- stdout and stderr;
- traceback length;
- figure count and dimensions;
- total generated artifact bytes;
- scratch-directory size.

Truncate in the worker before constructing an enormous Python `repr` when
possible. Include an explicit truncation marker.

Never JSON-encode large numerical arrays for the model or transport. If the
agent evaluates a huge tensor as its last expression, show a dtype/shape and a
bounded NumPy-style summary.

---

## 15. Model-facing output

The model-facing result is notebook-native, not a diagnostic JSON envelope.

Depending on the cell, the model receives:

1. captured stdout;
2. captured stderr;
3. the last-expression representation;
4. a traceback when execution failed;
5. generated figures as image parts.

Example presentation:

```text
128
```

or:

```text
stdout:
selected threshold 0.418

{'median_lag_ms': 31.4, 'p90_lag_ms': 58.7}
```

or a traceback plus the output emitted before failure.

The host may retain internal metadata such as:

- execution ID;
- duration;
- binding revision;
- kernel generation;
- namespace delta;
- artifact IDs;
- whether a signal interrupt preserved state.

That bookkeeping is for orchestration, UI, diagnostics, and tests. It is not
dumped into the model context on every call.

Only exceptional state information becomes model-visible:

- “The Python kernel was restarted; variables from earlier cells were lost.”
- “The selected track/window changed; `luma` now refers to the new selection.”
- a specific sandbox denial the agent can recover from.

Even those should be concise prose, not a large status object.

Namespace discovery is available on demand through normal Python helpers such
as `dir()`, `globals()`, and `luma.catalog()`.

---

## 16. Cancellation and resource control

### 16.1 Cancellation contract

The model-turn abort signal propagates to the active Python cell and to any
host call currently serving that cell. A host handler inherits the cell's
deadline and cancellation token. Read-only calls and track validation remain
cancellable; dropping one of those futures drops its open transaction, so
uncommitted work rolls back.

`track.apply` has one explicit commit barrier. After the cancellable compile
and validation pass, the host atomically chooses whether cancellation or the
write begins first. If cancellation wins, no write starts. If the write wins,
Rust awaits the transaction through commit and flushes its correlated,
authoritative host response before releasing the barrier. A Stop received in
that interval remains pending and interrupts Python immediately afterward; it
does not shield the rest of the cell. The UI reloads the authoritative score
after every execution outcome, so a committed edit is observable even if the
pending interrupt lands before Python consumes the response.

The command claims its cancellation token before resolving scope or assembling
bindings, so stop covers the entire cell request rather than only time spent in
Python. If cancellation is already set when execution reaches the workspace,
the workspace returns `interrupted` without launching or touching Python. It
checks again after a cold worker has started, preserving that fresh namespace
for the next cell instead of killing it. A final check before writing the
`exec` request closes the remaining pre-dispatch gap.

Once an `exec` request has been written, cancellation remains pending until the
worker emits the matching `started` acknowledgement. Only then may Rust send
the cancellation `SIGINT`. Each execution has an ID, and both the start frame
and later result frames are correlated to it, so a late cancel or frame cannot
interrupt the following cell.

### 16.2 Escalation ladder

Spawn the worker in a new process group/session. Pre-execution cancellation
during binding assembly or worker startup follows the short-circuit path above:
no user code runs, no signal is needed, and the namespace survives.

For a cancellation after the matching `started` frame, or an execution timeout:

1. send `SIGINT` to the worker process group;
2. allow roughly two seconds for a `KeyboardInterrupt` result;
3. if received, preserve the kernel namespace;
4. otherwise kill the process group;
5. mark the namespace lost;
6. surface a concise loss notice on the next agent-visible interaction if the
   current turn is already gone.

Measurements on this machine showed that `SIGINT` interrupts Python loops,
sleep, and many NumPy operations while preserving state. A single long native C
call may delay signal handling until it returns, which is why forced kill
remains necessary.

### 16.3 Limits

The host currently enforces:

- wall-clock cell timeout;
- a shorter per-host-call ceiling inherited from the cell deadline;
- host-call count and serialized-payload caps;
- a maximum complete-candidate clip count and byte size, checked before taking
  SQLite's write reservation;
- output byte caps;
- figure/artifact caps;

Production additionally requires:

- a hard aggregate scratch/workspace byte quota;
- a child-process count limit;
- aggregate memory and CPU controls for the worker process tree;
- GPU restrictions if libraries expose uncontrolled GPU allocation.

Those process-tree and storage controls are not implemented in v0. The macOS
filesystem/network sandbox and wall-clock cancellation materially constrain
authority, but they do not prevent a fork bomb, memory exhaustion, or filling
the allowed workspace. Until the remaining limits are enforced and tested, the
executor is a developer/v0 facility and must not be represented as
production-safe.

The initial wall-clock ceiling may use 90 seconds, matching comparable code
execution tools, and can be tuned from real agent traces.

---

## 17. Sandbox and host-capability security

### 17.1 Code execution is an authority boundary

Arbitrary Python execution is intentionally remote code execution by the model.
The question is what authority that code receives.

Immutable bindings are not sufficient. Unsandboxed Python can still:

- read or delete arbitrary user files;
- read Luma databases and credentials;
- inspect inherited environment secrets;
- open sockets;
- launch subprocesses;
- persist modifications outside Luma;
- exhaust resources.

Network denial alone is not enough. Code could read `~/.ssh/id_rsa`, print it,
and the tool result would be sent back through the remote model request. The
model/tool channel is itself an egress path.

### 17.2 Production capability policy

Allow:

- read/execute the bundled interpreter and managed venv;
- read the current workspace's explicit `inputs/`;
- write only the current workspace's `scratch/` and controlled output area;
- return bounded text and registered artifacts to the host.
- invoke only the narrow named host methods installed for the current trusted
  thread scope. An exact score/track/venue scope permits `track.render`;
  `track.check` and `track.apply` additionally require authenticated owner
  authority.

Deny:

- network;
- home-directory reads;
- unrelated track inputs;
- app databases, app config, auth material, and credentials;
- writes to the interpreter, venv, inputs, app, PATH entries, or settings;
- arbitrary subprocess execution where analysis libraries do not require it;
- environment inheritance beyond a small allowlist.

The sandbox must resolve symlinks and must not let a writable path widen the
policy.

### 17.3 Application mutation boundary

Python receives copies or read-only mappings, never live mutable Rust/JS
objects. Even if agent code mutates its local Python object, the host reinstalls
the canonical `luma` binding on the next cell. Ordinary Python assignments do
not change application state.

The track facade is a capability object, not a live model object. Its candidate
is local Python data. Only `check`, `render`, and `apply` send a complete
candidate through the worker's synchronous host-call seam. The host handler is
constructed from durable-thread scope and authenticated user state; the model
cannot select a different track, venue, score, user, or workspace, install a
handler, or widen its method allowlist. A validated exact track scope installs
read-only rendering even when the current user is not the score owner. The
separate edit capability exists only for the authenticated owner, and both
`check` and `apply` reject calls without it. The transaction service
independently rechecks scope and ownership, so the descriptive
`luma.track.editable` bit is never the security boundary.

The leading underscore on `_luma_host_call` is API hygiene, not protection:
arbitrary executed code can call it directly. Therefore every handler treats
its method and payload as hostile input, validates the complete candidate, and
derives all authority server-side.

Graph proposals and any future capabilities require their own explicit,
independently testable host policy. The worker protocol remains domain-neutral;
it routes a named request to an installed handler and grants nothing when no
handler is present.

### 17.4 macOS

Use a subprocess Seatbelt profile via `sandbox-exec`:

- no network;
- explicit read/execute roots;
- per-workspace read/write roots;
- environment scrub;
- process restrictions.

`sandbox-exec` is deprecated but has no published removal timeline or supported
replacement for this desktop use case. Keep profile generation behind a
swappable launcher module.

Packaging considerations:

- the bundled Python executable and native extensions must work under hardened
  runtime/notarization;
- library-validation entitlements apply to the child executable, not merely the
  Tauri parent;
- do not convert the whole Tauri app to App Sandbox as a shortcut;
- use the non-GUI Matplotlib backend.

### 17.5 Linux

Use Landlock for filesystem restrictions and seccomp for syscall/network
restrictions, launched directly from Rust.

Do not require bubblewrap, Docker, root setup, or distro-specific AppArmor
configuration in a consumer desktop app.

### 17.6 Windows

Do not ship this capability on native Windows until an AppContainer/job-object
or equivalent sandbox meets the same policy without an elevated developer
setup. WSL is not an acceptable consumer-app dependency.

### 17.7 Failure behavior

In a production build, sandbox initialization failure is a hard stop for the
Python tool. Do not warn and continue unsandboxed.

An unsandboxed launcher may exist only behind an explicit developer-only build
or feature flag for local experiments.

### 17.8 Existing JS probe

The current Web Worker is not network-isolated by construction; workers can use
`fetch` and `importScripts`. While it remains during migration, restrict its
network access through the app's CSP. Delete it after the Python graph path is
proven rather than retaining two executor systems.

---

## 18. Track authoring in Python

The agent does not read or edit a source file. The complete authored lighting
timeline is part of the ordinary `luma.track` binding, and the only track
authoring surface is a staged Python candidate:

```python
edit = luma.track.edit()
edit.add_clip(...)
edit.update_clip(...)
edit.remove_clip(...)

view = edit.window(bars=(49, 65))  # half-open: bars 49 through 64
view.timeline()
view.output.heatmap()

edit.diff()
edit.check()
edit.apply()
```

This is one system, not a Python representation plus a separate agent file
format. The same `luma` tree supplies audio, musical features, venue data,
patterns, arguments, the current clip document, and rendered candidate output.
The model uses ordinary Python to combine them creatively.

### 18.1 Complete lossless snapshot

`luma.track.clips` contains every clip in the selected authored track, not only
the current viewport or recently touched section. The snapshot preserves every
authored semantic value:

- stable clip identity;
- stable pattern identity, independent of a duplicate or renamed display name;
- exact start and end seconds;
- exact, sparse, and negative `z` values;
- blend mode;
- every argument value, including palettes, gradients, non-global selections,
  unknown/legacy arguments, and JSON values absent from the current pattern
  schema.

Database timestamps, ownership/sync bookkeeping, caches, and editor selection
are not authored semantics and do not enter the candidate. A semantic hash of
the authored values becomes `luma.track.revision`; row order and JSON object-key
order do not affect it.

Pattern and argument display names are conveniences. A unique display name may
be resolved for ergonomic authoring, but duplicate names are an error and the
candidate wire format always carries stable IDs. Existing unknown argument IDs
and legacy JSON values are preserved rather than normalized away.

### 18.2 Staged full-candidate API

Calling `luma.track.edit()` succeeds only when `luma.track.editable` is true,
which means the trusted host resolved authenticated owner authority for the
exact score/track/venue scope. It captures both the complete base snapshot and
its revision. All mutations are local until `apply()`:

```python
edit = luma.track.edit()

clip = edit.add_clip(
    "Verse wash",                 # stable ID or unique display name
    bars=(49, 57),                # or seconds=(start_s, end_s)
    z=0,
    blend="replace",
    args={"Intensity": 0.7},     # argument ID or unique display name
    selection="front_wash",      # shorthand when exactly one Selection arg exists
)

edit.update_clip(
    clip.id,
    bars=(49, 65),
    z=2,
    args={"Intensity": 0.85},
    unset_args=("Old override",),
)

edit.remove_clip("persisted-clip-id")
```

Exactly one of `bars=(start, end)` or `seconds=(start, end)` specifies a range.
Bar numbers are 1-indexed musical boundaries derived from downbeats and ranges
are half-open. `args` on update merges into existing arguments; `unset_args`
removes named overrides deliberately. Changing a clip to another pattern does
not carry old pattern arguments into the new schema.

Clips on the same `z` use half-open overlap semantics. A draft cannot introduce
a new same-layer overlap, but a lossless edit may preserve an overlap already
present in legacy data.

An `Edit` always contains the **whole candidate track**, including unchanged
clips. New clips receive recognizable temporary `new:*` IDs so they can be
updated, removed, plotted, and rendered before apply. Rust replaces those with
canonical UUIDs and returns the temporary-to-persisted ID map on success.

### 18.3 Explicit immutable candidate views

A visualization always begins with an explicit half-open range:

```python
view = edit.window(bars=(49, 65))
# or
view = edit.window(seconds=(120.0, 150.0))
```

Use `luma.track.window(...)` with the same explicit range to inspect the current
committed snapshot without first creating an edit. This path remains available
for a coherent read-only score/track/venue scope: it can produce both
`timeline()` and the real compositor `output.heatmap()` without mutation
authority. `edit()`, `check()`, and `apply()` remain unavailable without the
separate owner capability.

The view snapshots the full candidate at that moment. Subsequent draft changes
require a new view, so a timeline and heatmap cannot silently describe
different candidates. Clips outside the window remain in the candidate; clips
intersecting the window appear even when they were unchanged or begin outside
it.

`view.timeline()` renders the **authored structure** as an image with time on
the x-axis and explicit `z` on the y-axis. It answers which patterns overlap and
how they stack; it is not a text serialization and not a composited result.

`view.output` lazily asks the authoritative host to render that exact candidate
and interval through the real Luma compositor. Its tensor is:

```text
[light, time, RGB]
```

The axes carry stable venue light IDs, exact sampled absolute seconds, and
`r/g/b` channel labels. Values are normalized linear color multiplied by
dimmer, matching Luma's single composited light concept; dimmer is not exposed
as a parallel output plane. Clips retain their original full span when the
window begins mid-clip so span-relative pattern phase remains correct.

The current sampling policy is finite and explicit: 16 samples per beat when a
valid BPM exists, otherwise 32 samples per second, with the sample count clamped
to `[2, 2048]`. Sampling is half-open, so the first requested time is included
and the window end is not. The tensor publishes the exact `f32` times passed to
the evaluator. Windows long enough to hit the 2,048-sample cap have lower
temporal density; agents should inspect smaller windows when onset-level detail
matters.

`view.output.heatmap()` renders that tensor with time on x, light on y, and the
pixel color equal to the final composited RGB. This intentionally avoids camera
placement and scene-renderer ambiguity while preserving the two dimensions an
agent must understand. Candidate tensors and figures use the same artifact
store as every other Python input/output.

### 18.4 Diff, authoritative check, and atomic apply

`edit.diff()` is a semantic local diff with added, updated, and removed clips.
`edit.check()` performs cheap local validation, then sends the complete
candidate and base revision to the host. The host:

1. re-resolves and verifies the durable thread's score/track/venue/user scope;
2. rejects a stale base revision;
3. validates IDs, patterns, time ranges, arguments, and overlap invariants;
4. strictly compiles every pattern in the complete candidate;
5. returns a structured non-mutating result.

Candidate rendering repeats authoritative scope, revision, and semantic checks,
then strictly compiles and composites the clips intersecting the requested
window. Strict candidate compilation always rebuilds from authoritative pattern,
beat-grid, group, and venue inputs; it neither reads nor populates the live
renderer's incremental plan cache. Compile failures are errors, and preview
must not inherit the live compositor's tolerance for broken legacy clips.

That strict compile is an authoritative **snapshot**, not a lock over all of
those dependencies. For apply, it runs immediately before the score
transaction. The score CAS below atomically protects the authored clip document
only; it does not lock or fingerprint pattern graphs, venue patching, groups, or
beat-grid data between compile and commit. Those inputs can change in that
interval, and pattern graphs may also change normally after the score commits
because clips intentionally reference durable pattern IDs. If Luma later needs
an immutable, reproducible show package, dependency fingerprints and a broader
snapshot/version contract are separate work.

Every `edit.apply()`, including a candidate with no semantic diff, sends the
same complete candidate and base revision to the host. The trusted command
layer derives scope from the durable thread and derives the current user from
authenticated application state; caller-supplied IDs can be omitted but cannot
retarget the operation. The edit service rechecks exact ownership and scope,
acquires `BEGIN IMMEDIATE`, compares both the authored head and the semantic
**score revision** inside that write transaction,
repeats score validation, assigns IDs/timestamps/ownership for new rows, writes
only the semantic projection diff, serializes the complete canonical
`score.luma`, and advances the relational head by CAS. The immutable revision,
projection changes, operation outcome, and sync proposal commit together. A
zero-diff result therefore still asserts the authoritative current revision and
returns the complete canonical document, with zero change counts and
`applied=False`. A failure changes neither projection nor history. A concurrent
human or agent score edit produces a conflict instead of a blind overwrite.

Unchanged rows retain their persistence metadata. A successful result contains
the new revision, complete canonical clips, ID map, and change counts; the next
cell receives that document through the normal refreshed `luma.track` binding.

### 18.5 `score.luma` is the canonical revision file

`score.luma` is the human-readable, lossless score file stored as exact bytes
in every score revision. Import/export uses the same codec. The current Python
agent does not edit the file directly; its typed transaction is serialized
through this codec at the revision boundary. A filesystem subagent may edit a
materialized copy in its isolated directory, then invoke check, commit, and
merge through the authored-state service. Its compiler contract is:

```text
compile(export(authored_track)) == authored_track
```

The first line is the required canonical envelope
`# luma-score-schema: 1`. This is file-format metadata, not an authored score
comment. Unversioned and unknown-version revision files fail closed; serializers
never guess a historical grammar from content. Future format changes add an
explicit version decoder and ordered migration instead of reinterpreting old
revisions with the newest parser.

The committed form is deliberately stricter than the human/model authoring
grammar. Every canonical clip declares its stable clip ID, stable pattern ID,
exact `z`, exact `f64` start/end seconds, blend mode, and every argument as raw
JSON under its stable argument key. The pattern name beside its ID and attached
comments are presentation only. Canonical source never contains bar timing,
the parenthesized Selection shorthand, color/identifier shorthand, inferred
layers, missing identities, or duplicate clip IDs/argument assignments.

Musical timing, Selection expressions, typed value shorthand, argument names,
and omitted clip IDs remain useful at human/model ingress and in exemplars.
They are resolved exactly once against the current beat grid and pattern
interface before commit; the resulting `score.luma` contains only the
self-contained canonical form. The compiler must never skip a pattern,
normalize `z`, clamp a range, fill an absent override, discard an unknown
argument, round a large JSON integer, or choose the first duplicate name.

One Rust codec is authoritative for isolated-workspace ingest, relational
semantic merge, restore, import/export, and UI validation. The TypeScript UI is
a client of that codec, not a second parser. The codec preserves attached source comments when
rewriting a parsed document; canonical serialization from an existing database
snapshot has no comments to invent. Canonical file bytes never encode a
database head generation or CAS token.

Historical decoding is context-free. Reading a committed `score.luma` receives
no database handle, beat grid, pattern registry, or current implementation
interface. Restore decodes the selected tree directly. A typed three-way merge
decodes base, ours, and theirs directly, merges clips by stable identity, and
carries presentation names/comments from those source trees. Reprocessing a
track, renaming a pattern, changing or deleting its arguments, or temporarily
breaking its current graph therefore cannot reinterpret or make an old score
revision unreadable.

Import compiles the whole DSL before writing, materializes one
complete replacement document, preserves a supplied clip ID only when it
belongs to the current score, rejects foreign or duplicate IDs, and assigns a
client-only correlation ID to a clip whose DSL omits identity. It then submits
both the exact base snapshot it compiled from and the complete candidate to the
same relational full-document service.

Paste import, generated DSL, undo, and redo are adapters over the same canonical
authored-document transaction as Python and ordinary clip CRUD. The host derives
the base semantic revision from the supplied snapshot, rejects it when the head
changed, rewrites every genuinely new client ID to a draft ID, and delegates to
`AuthoredDocuments` for scope/owner checks, validation, host UUID/timestamp
allocation, lossless `score.luma` serialization, immutable revision creation,
SQLite projection, and head CAS. The returned ID map rebases UI history and
selection; the editor hydrates only the current authoritative projection after
success. A conflict leaves live UI state and undo history untouched. There is
one write foundation, not a permissive projection writer beside revision
history.

The user message that begins a model turn is persisted before the remote model
is called. Python `track.apply()` derives its durable operation identity from
that user-message ID plus the canonical host-resolved score scope and complete
edit plan. The immutable operation-outcome table is queried before stale-base
validation and is written atomically with the relational revision and
projection. An exact apply therefore replays after IPC loss, process restart,
or a regenerated provider tool-call ID; a different plan is a different
operation. Provider tool-call IDs, Python source text, and worker call ordinals
are deliberately not mutation identity.

This does not change the current model contract: the Python agent's one route
for both understanding and authoring remains `luma.track`.

---

## 19. Subagents

Subagent orchestration consumes the relational authored-state foundation; it
does not invent another proposal or checkpoint store.

This section specifies the enablement boundary, not a currently exposed app
feature. The isolated-workspace primitives are host-internal and independently
testable. Production exposure begins only when the same host component
owns child launch, sandbox policy, immutable process-tree lease, final snapshot,
and process-tree exit; exposing raw directory calls piecemeal is forbidden.

- an orchestrator thread allocates each child job a dedicated plain directory
  from an explicit base revision and records that base in SQLite;
- a child may also have its own durable thread/Python workspace, but transcript
  and kernel ownership are independent of the filesystem isolation primitive;
- child jobs never share a mutable Python namespace or authored directory;
- each child process tree holds a host-owned lease; check/commit/retire begins
  only after that lease is released and the whole process tree has exited;
- the sandbox exposes only canonical authored files plus bounded scratch/output,
  never SQLite, credentials, app configuration, or arbitrary host paths;
- workspace creation and commit are idempotent under caller operation IDs, and
  old retries cannot move a newer private workspace head backward;
- the host uses relational ancestor walking plus Luma's typed three-way merge;
- score clips merge by stable clip ID, graph nodes by node ID, edges by target
  input slot, and pattern arguments by stable argument ID;
- score comments/trivia merge by their stable annotation/layer/document
  attachment instead of being discarded by semantic serialization;
- divergent scalar edits, add/add-different, and delete/modify produce
  structured conflicts; layout conflicts never block a semantic merge;
- the authoritative score/graph validator runs before a two-parent revision
  can advance the live document head and projection;
- cancellation, cost limits, and UI streaming remain orchestration concerns.

Possible future uses include per-section track authoring, generate-and-judge
panels, adversarial verification, and per-venue robustness checks. None is
required to validate the core executor.

---

## 20. Component boundaries

Suggested module shape:

```text
src-tauri/src/agent_execution/
  mod.rs
  workspace.rs             thread -> workspace registry
  worker_process.rs        protocol, process, interrupt
  worker_launcher.rs       sandbox-independent launcher trait
  track_host.rs            scoped check/render/apply host capability
  bindings/
    mod.rs
    manifest.rs            BindingValue, TensorRef, AxisSpec
    assembler.rs           provider composition and validation
    providers/
      track.rs
      audio.rs
      features.rs
      venue.rs
      patterns.rs
      graph.rs
  artifacts/
    mod.rs
    store.rs
    codecs.rs
  sandbox/
    mod.rs
    macos.rs
    linux.rs

src-tauri/python/
  luma_exec/
    worker.py               persistent cell loop and generic host-call bridge
    bindings.py             manifest -> Python namespace
    track.py                Track/Edit/Window/Output facade

src-tauri/src/services/
  track_edits.rs            typed score validation and SQLite projection
  score_dsl/                one lossless score parser/serializer/compiler
  authored_state/           relational revision DAG/hash/CAS primitives
  authored_documents.rs     sole revision/projection/lifecycle authority
  authored_documents/
    operations.rs           atomic outcomes and sync enqueue
    turns.rs                prepare/finalize/recovery/history/restore
    workspaces.rs           plain isolated subagent directories
  authored_merge.rs         typed score/graph three-way merge
  authored_sync_merge.rs    total deterministic device convergence
  graph_documents.rs        graph revision, validation, atomic projection
  score_mutations.rs        shared command-shaped score adapters

src-tauri/src/sync/
  authored_remote.rs        typed boundary for exactly three head/archive RPCs
  registry.rs               immutable authored + transcript row mappings

src/shared/lib/agent/
  threads.ts                durable thread client and scoped resolution
  python-tool.ts            shared Python tool adapter

src/shared/components/agent-chat/
  create-agent-chat.ts      shared chat/session lifecycle

src/features/track-editor/agent/
  track-agent.ts            Python-only track tool vocabulary and UI refresh
  build-context.ts          concise Python-first system prompt
```

The exact filenames may follow repository conventions, but the dependency
direction should remain:

```text
domain providers -> binding manifest -> worker
artifact store -----------------------^

workspace registry -> worker process -> launcher/sandbox

Track/Edit facade -> generic host call -> scoped track host
                                              |
                                              v
                                      authored documents
                                        |          |
                                  typed codecs   compositor

agent adapters -> workspace service
```

No Tauri dependency is needed inside:

- manifest validation;
- artifact codecs;
- Python protocol implementation;
- worker process state machine;
- sandbox profile generation.

The Python `Track` facade is independently testable with ordinary binding
values and a fake synchronous host callback. Revision-DAG primitives, strict
typed merges, total sync merges, workspace lifecycle, score/graph projection,
and orchestration are independently testable.
The generic worker protocol knows nothing about tracks, and the track host
knows nothing about model messages.

Tauri commands are thin adapters.

### 20.1 Headless hosts

Two binaries serve this data plane to a process that is not the desktop app.
Both are thin adapters over `luma_lib::dispatch` — the seam the
`#[tauri::command]` wrappers sit on — so neither owns scope resolution, the
binding manifest, the write-admission gate, or the interrupt ladder. Both boot
through `luma_lib::headless_host`: same flags (`--config-dir`,
`--fixtures-root`, `--cache-dir`, `--fixture-principal`), same migrations, same
managed-venv workspace service, same startup recovery of half-deleted threads.
Events go to stderr, because both put their protocol on stdout.

| | |
|---|---|
| `src-tauri/src/bin/agent_harness.rs` | one JSON request per line; the shim in `scripts/headless/shim.ts` puts `window.__TAURI_INTERNALS__.invoke` on top of it so unmodified frontend agent code runs under Bun |
| `src-tauri/src/bin/luma-mcp.rs` | MCP over stdio, so an out-of-process coding agent gets the `python` tool itself |

`luma-mcp` exposes four tools:

```text
open   {track_id | track_query, venue_id?}  the bound namespace's catalog
python {code}                               stdout/repr/traceback + figures
reset  {}                                   a fresh workspace and kernel
cancel {}                                   interrupt the cell in flight
```

`open` with no arguments lists the library instead of binding it. The
difference from the in-app tool is the *session*: an MCP client has no editor
to read a track from, so `open` resolves the track (and the venue and score
that make `luma.venue` and `luma.track` real — a venue scope is only legible
together with a score, §10.2), creates one durable `track_copilot` thread
pinned to it, appends one user message for cells to be attributed to, and
returns `luma.catalog()`. Every later call addresses that thread, so `python`
takes only code, exactly as §7.1 requires. `reset` deletes the thread — which
is what takes the workspace and kernel with it — and opens the same scope
again.

`python`'s description is `PYTHON_TOOL_DESCRIPTION`, and its result is
`agent::tools::python::cell_content_blocks` — the same text and the same
projection the in-app tool gives its model, clamping and figure budget
included. A second wording or a second projection would be a second tool.

The loop is concurrent, one task per request, for the reason the JSON-RPC
harness's is: `cancel` exists precisely to interrupt a `python` that is still
in flight. The framing, `initialize`, `ping` and `tools/list` live in
`src-tauri/crates/mcp-stdio`, shared with the GPUI harness's server; the loop
does not, because that harness is deliberately serial.

The sandbox is not a flag on these hosts. They resolve the worker environment
through `agent_execution::headless_env`, so `sandbox::default_launcher` decides,
and §17.7 still holds: a release build cannot reach the passthrough at all, and
a debug build only with `LUMA_UNSANDBOXED_PYTHON=1`.

Register the server with a `.mcp.json` at the repository root (not committed):

```json
{
  "mcpServers": {
    "luma": {
      "command": "./src-tauri/target/debug/luma-mcp",
      "args": []
    }
  }
}
```

Add `["--config-dir", "/path/to/scratch"]` to work against a disposable copy of
the library rather than the real one. `scripts/headless/mcp_smoke.ts` drives the
whole protocol against such a copy.

---

## 21. Testing requirements

### 21.1 Durable threads

- two threads for one track have different IDs and workspaces;
- full tool history survives round-trip persistence;
- New Conversation preserves the old transcript and authored revisions while
  allocating a distinct thread/Python workspace;
- transcript append requires the expected local head, rejects a stale sibling,
  and exact operation replay returns the original immutable message range;
- state-and-conversation restore creates a new thread at the selected message,
  shares the prefix without copying or editing nodes, and leaves the original
  thread complete;
- threads, messages, append receipts, server transcript heads, preparations,
  outcomes, forks, and deletion receipts round-trip through Supabase;
- terminal thread deletion cannot be undone by a later pulled lifecycle/head
  row, while immutable trace nodes remain readable;
- an out-of-order conversation lookup cannot activate after a newer selection;
- the first cell after an app restart reports that the prior kernel namespace
  was lost;
- thread deletion cleans up its workspace.

### 21.2 Relational authored state and sync

- revision IDs, manifest hashes, file hashes, exact file sets, ordered parent
  closure, and acyclic ancestry are independently verified in SQLite and
  Postgres;
- revision/files/parents/outcomes are immutable and permanent; exact row replay
  succeeds while identity collision, update, or deletion fails;
- every runtime score and graph writer creates relational history through the
  same authority; direct projection writers are unreachable from adapters;
- revision, validated projection, operation outcome, head CAS, and sync enqueue
  are one SQLite transaction, with crash tests at every statement boundary;
- prepare followed by durable assistant persistence recovers exactly once;
- response-loss retries return the original outcome and never rewind a newer
  head or private workspace tip;
- a concurrent live-head advance during turn/workspace finalization is
  strict-merged; typed conflict is durable, never projected, and does not block
  the next turn;
- both restore modes create a forward revision, and old/superseded history
  remains selectable beyond the UI's pagination limit;
- isolated directories reject traversal, symlinks, extra paths, size overflow,
  stale workspace heads, and changed snapshot hashes; commit canonicalizes the
  exact bounded file set and retire deletes only the disposable directory;
- score and graph merges cover add/add, delete/modify, concurrent scalar edits,
  stable identities, graph input slots, dangling dependencies, comments/trivia,
  and non-blocking graph layout;
- Supabase exposes exactly the three named RPCs, RLS prevents cross-principal
  access, clients cannot write server head/outcome projections, and immutable
  triggers reject mutation/deletion;
- one device can submit and disappear permanently while another authenticated
  owner client pulls and terminally integrates its proposal;
- concurrent proposals converge in `server_proposal_seq` order on every device;
  stale integration recomputes and no client timestamp participates;
- structural sync merges preserve independent clip/arg/node/param edits and
  choose the later proposal for overlapping semantic fields;
- a composition-created graph cycle falls back to the whole valid proposal; an
  invalid/unreadable proposal becomes `quarantined_noop`; missing or ambiguous
  merge base takes the same terminal fallback, so integration never blocks;
- archived documents cancel every pending proposal and cannot be resurrected by
  a pulled document, head, catalog, or projection row;
- superseded proposal tips appear in state history and can be restored;
- `sync_seq` pull tests include a late long-running transaction and prove the
  cursor cannot skip a row that commits after a higher client-observed time.

### 21.3 Binding manifest

- schema-version checks;
- deterministic provider merge;
- duplicate-path rejection;
- shape and byte-length validation;
- axis count/shape validation;
- unit and time-origin validation;
- unavailable versus empty values;
- provenance preservation.

### 21.4 Semantic alignment

- graph view times exactly match evaluator times;
- primitive-indexed graph views exactly match evaluator ID ordering;
- broadcast or mismatched graph taps expose an unlabeled index axis and cannot
  be mistaken for venue-aligned rows;
- positions and labeled views reject mismatched primitive identities;
- audio and graph time axes correlate in absolute seconds;
- mel and band axes cover their real data dimensions.

### 21.5 Artifact codecs

- PCM header/offset/sample-rate/channel parsing;
- NPY loading;
- raw little-endian dtype/shape loading;
- read-only NumPy mappings;
- unaligned PCM offset correctness;
- input path ownership;
- artifact leases and cleanup;
- current bounded-base64 figure persistence and oversized-figure placeholders;
- durable artifact registry/reference restoration once artifact-only transcript
  persistence is implemented;
- path traversal and symlink rejection.

### 21.6 Kernel semantics

- variable persists across cells;
- `luma` changes across binding revisions while user variables persist;
- reassigning `luma` is repaired before the next cell;
- each cell that reaches user-code entry emits exactly one correlated `started`
  frame after binding installation and syntax parsing, before any user
  statement;
- last-expression display;
- stdout and stderr capture;
- native fd-level stdout cannot corrupt protocol frames;
- exceptions preserve prior stdout;
- figures are captured and closed;
- huge representations are bounded.

### 21.7 Interruption

- cancellation during async binding assembly short-circuits before Python and
  preserves the existing namespace;
- cancellation during cold worker startup prevents the cell from running and
  leaves the freshly started kernel reusable;
- for an executable cell, cancellation after an `exec` write but before
  `started` is held until the matching acknowledgement, then interrupts the
  intended cell;
- Python loop interrupted with namespace preserved;
- sleep interrupted;
- representative NumPy operation interrupted where CPython permits;
- forced timeout kills the process group;
- forced kill produces a state-loss notice;
- late cancellation cannot hit the next execution;
- cancellation before the track commit barrier prevents the write;
- cancellation during a track commit waits for the authoritative host response
  and then interrupts any remaining cell code without losing the kernel;
- model-turn stop propagates to the cell.

### 21.8 Sandbox

Platform acceptance tests must prove:

- current input artifacts are readable;
- scratch is writable;
- app databases are unreadable;
- home credentials are unreadable;
- input artifacts are not writable;
- network access fails;
- environment secrets are absent;
- disallowed subprocesses fail;
- denial errors identify the rejected capability.

### 21.9 Track candidate and transaction

- the bound clip snapshot round-trips exact IDs, times, sparse/negative `z`,
  blend mode, and arbitrary legacy JSON arguments;
- `add_clip`, `update_clip`, and `remove_clip` mutate only the local full
  candidate;
- pattern and argument display names resolve only when unique;
- a new clip's temporary ID can be updated, removed, rendered, and mapped to a
  host UUID on apply;
- bar and second ranges are half-open and require exactly one coordinate form;
- a window remains immutable after later draft mutations and includes unchanged
  clips that intersect it;
- the timeline image maps authored time to `z` without pretending to be output;
- candidate rendering uses the real strict compositor, stable light IDs, exact
  sampled times, and `[light,time,RGB]` values equal to color multiplied by
  dimmer;
- render sampling is half-open, follows the 16-samples-per-beat/32-Hz-fallback
  policy, and lowers density rather than exceeding 2,048 samples;
- `check()` is non-mutating and reports strict compile failures;
- an exact read-only track scope can render committed timeline and compositor
  heatmaps but cannot create an edit or call `check()`/`apply()` successfully;
- caller-supplied scope cannot retarget the durable thread;
- stale base revisions conflict under concurrent edits;
- invalid apply leaves every row unchanged;
- successful apply is atomic, maps temporary IDs, and preserves unchanged-row
  persistence metadata;
- a no-diff apply still performs host authorization and revision CAS, returns
  the authoritative document, and reports zero counts with `applied=False`;
- human DSL import preserves valid existing IDs, rejects foreign/duplicate IDs,
  materializes new identities, and replaces the score with one atomic
  diff-based database operation;
- a real worker-to-host-to-SQLite integration test applies a candidate and the
  next cell sees the refreshed track while retaining prior Python variables.

---

## 22. Acceptance criteria

The design is implemented when all of the following are true:

1. Both agents operate on durable thread IDs and structured tool history.
2. Both agents expose the same `python` tool contract.
3. A variable defined in one cell is usable in a later turn of the same thread.
4. A different thread cannot access that variable.
5. `luma` refreshes after authored-track, graph, selection, or analysis changes
   without clearing agent variables.
6. The track agent can compute directly over precomputed drum onsets.
7. The track agent can independently compute over the audio mix or any stem.
8. The graph agent can compare graph-view peaks against drum onsets in one cell.
9. Graph tensors include exact time and channel axes; primitive-indexed views
   carry exact ordered IDs, while broadcast/mismatched taps are explicitly
   unlabeled.
10. Venue positions align only with labeled primitive identity, not merely row
    count.
11. The agent can produce and see a Matplotlib figure.
12. The model-facing result is notebook-native rather than a bookkeeping JSON
    object.
13. No large numerical array crosses through JSON lists or permanent base64.
14. Graph, audio, and feature inputs use one binding/artifact mechanism.
15. The track agent has one model-facing tool, persistent Python; it does not
    expose per-operation clip tools or an agent-editable `score.luma` file.
16. `luma.track` contains the complete lossless clip snapshot, semantic
    revision, and editability bit; there is no parallel timeline branch.
17. `luma.track.edit()` exposes exactly the coherent staged operations needed
    to add, update, and remove clips over a full candidate.
18. Candidate visualization requires an explicit immutable half-open window;
    the authored timeline maps time × `z`, and the output heatmap maps time ×
    light from the actual compositor. Both remain available to an exact
    read-only score/track/venue scope.
19. Candidate output is an artifact-backed semantic tensor with shape
    `[light,time,RGB]`, stable light identities, exact times, and RGB already
    multiplied by dimmer.
20. Diff and check are non-mutating; check uses authoritative current scope and
    strict graph compilation.
21. Every apply sends the complete candidate plus base revision through the
    sole relational revision/projection authority; a no-diff apply still
    asserts the revision and returns `applied=False` with the authoritative
    document.
22. Track mutation scope and current-user ownership come from the durable thread
    and trusted host, never from model-selected IDs; `check` and `apply` require
    that owner capability even though timeline and compositor reads do not.
23. Python has no generic application mutation, database, filesystem, or Tauri
    authority beyond explicitly installed host capabilities.
24. A new conversation/thread cannot inherit the prior thread's Python
    namespace.
25. Cancellation covers binding assembly, cold startup, dispatch, host calls,
    and running user code. Pre-execution cancellation runs no user code and
    preserves the namespace; cancellation-driven `SIGINT` is sent only after
    the matching `started` acknowledgement; forced process death reports state
    loss.
26. Production execution cannot read home/app secrets, write outside scratch,
    or access the network.
27. Sandbox failure disables the tool rather than running with broader access.
28. The existing JS graph probe is deleted after Python parity is established.
29. Figure transcripts retain durable artifact references instead of persisted
    base64, and those references replay after app restart.
30. Artifact metadata is restored or reconciled after app restart, and the
    first new kernel reports loss of the prior live namespace.
31. Human DSL import preserves valid existing clip identities and replaces the
    complete score through one atomic relational revision transaction; it is a
    trusted UI operation, not the agent's base-revision protocol.
32. Every completed assistant message has a durable prepared, committed, or
    conflicted authored outcome; crash recovery never guesses from current UI
    state.
33. Restore and clean subagent merge create ordinary forward relational
    revisions using the same typed validation, head CAS, and projection path as
    direct edits; subagent conflicts are stored and returned as typed data.
34. Authored scores, graphs, their complete immutable revision DAGs, operation
    outcomes, and superseded proposal tips synchronize through the existing
    row-sync engine; `track_scores` and graph payload projections do not form a
    second sync authority.
35. The Postgres migration ships DDL, owner RLS, server-side immutability and
    closure checks, and exactly the three public RPCs named in §5.11.
36. Any online authenticated owner client can integrate any pending proposal;
    a permanently offline origin device cannot wedge the document.
37. Device integration is silent and deterministic: server order chooses the
    later writer, structural composition preserves independent edits, and no
    sync conflict modal or agent conflict record is produced.
38. Integration is total: an invalid structural result, including a graph
    cycle, selects the whole valid proposal; an invalid/unreadable proposal
    terminally keeps current as `quarantined_noop`; no semantic failure leaves
    a proposal pending.
39. Superseded proposal tips are visible, labeled, paginated, and restorable in
    state history.
40. State-only restore leaves the conversation unchanged. State-and-conversation
    restore creates a new thread sharing the selected immutable transcript
    prefix; the original thread and every message remain intact.
41. Threads, transcript nodes, append receipts, server transcript heads, turn
    preparations/outcomes, forks, and deletion receipts back up to Supabase;
    terminal deletion cannot be undone by a later pull.
42. Pull cursors use commit-ordered server `sync_seq`, never a client timestamp
    or `updated_at`, and cannot miss a transaction that commits late.
43. Archived authored documents are terminal across SQLite, Postgres, and pull:
    sync cannot recreate their head or live projection, while all immutable
    history remains readable.

---

## 23. Current assets and measured constraints

These findings informed the design and should prevent repeated research.

### 23.1 Existing Python environment

Luma already ships:

- bundled CPython 3.12;
- a managed venv under the app cache;
- interpreter validation and environment creation;
- requirements hashing and cached installation;
- machine-aware PyTorch installation;
- one-shot workers for beats, roots, stems, MERT, drum onsets, and bar
  classification;
- NumPy, SciPy transitively, librosa, matplotlib, soundfile, Demucs, and the
  other preprocessing dependencies.

This removes interpreter/dependency bootstrap work. It does not remove the need
for the new persistent worker, data plane, or sandbox.

### 23.2 Data already available

- `track_beats`: beats, downbeats, BPM, downbeat offset, beats per bar;
- `track_drum_onsets`: kick, snare, hat, and cymbal times;
- `track_bar_classifications`: per-bar intensity and tag probabilities;
- `track_roots`: chord-section start/end/root;
- `track_stems`: drums, bass, vocals, other;
- `track_waveforms`: low/mid/high band envelopes;
- track mel spectrogram generation;
- MERT full-mix and drum arrays;
- graph run views, mel views, universe state;
- evaluator positions and primitive ordering;
- current graph definition and pattern arguments;
- timeline annotations, pattern summaries, and argument schemas;
- venue fixtures, stage pieces, and group data.

### 23.3 Import timings measured on this machine

Warm page cache, bundled app venv:

| Operation | Approximate wall time |
|---|---:|
| Bare interpreter | 0.01 s |
| `import numpy` | 0.05 s |
| `import scipy.signal` | 0.51 s |
| `import matplotlib.pyplot` | 0.28 s |
| `import librosa` | 0.02 s |
| first `librosa` hot attribute use | about 0.90 s |
| `import torch` | about 0.84 s |

Librosa uses lazy loading. Prewarming must touch the attributes the agent is
likely to use; importing the top-level package alone merely moves the delay.

### 23.4 Do not fork-prewarm

Forking after importing pyplot crashed deterministically on macOS due to ObjC
runtime initialization. Native numerical libraries may also initialize thread
pools that are unsafe after fork.

Spawn a fresh persistent process and pay the roughly one- to two-second warmup
once. Do not use a preloaded fork server.

### 23.5 Array transport measurements

For a 20 MB float32 array:

| Transport | Measured cost/size |
|---|---|
| JSON list | about 1.87 s, 103 MB |
| base64 raw bytes | about 22 ms, 26.7 MB |
| `np.save` | about 3.3 ms, 20 MB |
| `np.load` plus sum | about 4.2 ms |
| mmap open | about 8.7 ms |

Never use JSON lists for large arrays. Mmap is not automatically faster for
small/medium arrays; the loader may choose full read versus mapping.

### 23.6 Interrupt measurements

`SIGINT` delivered to a worker process group:

- interrupted a Python infinite loop at about 0.5 seconds;
- interrupted `sleep`;
- interrupted repeated matrix multiplication while preserving prior state;
- was delayed by about two seconds during one long native matrix multiply.

This supports the SIGINT-then-kill policy.

### 23.7 Model configuration already changed

Orthogonal to this executor design:

- track copilot model: `anthropic/claude-opus-5`;
- graph agent model: `x-ai/grok-4.5`;
- graph reasoning effort: `high`;
- track reasoning effort currently remains `medium`;
- venue expert remains `moonshotai/kimi-k2.6:nitro`.

These choices do not affect the executor interfaces.

---

## 24. Deferred product capabilities

The following are compatible with this foundation but are not part of the core
executor:

- real-renderer stage filmstrips;
- deterministic rig-safety linting;
- agent access to evaluator goldens;
- reference-video-driven authoring;
- persistent pattern memory;
- live StageLinQ/ProDJ Link operation;
- multi-agent section authoring and judging.

They should consume the same durable threads, binding revisions, artifact
store, and scoped host-capability boundary rather than creating parallel
systems.

---

## 25. Reference sources from the research

Primary/comparable systems:

- Anthropic code execution:
  <https://platform.claude.com/docs/en/agents-and-tools/tool-use/code-execution-tool>
- Claude Code sandboxing:
  <https://code.claude.com/docs/en/sandboxing>
- Anthropic sandboxing engineering:
  <https://www.anthropic.com/engineering/claude-code-sandboxing>
- Anthropic sandbox runtime:
  <https://github.com/anthropic-experimental/sandbox-runtime>
- Cursor agent sandboxing:
  <https://cursor.com/blog/agent-sandboxing>
- E2B persistence:
  <https://e2b.dev/docs/sandbox/persistence>
- Open Interpreter safety:
  <https://docs.openinterpreter.com/safety/introduction>

Threat model and platform references:

- The lethal trifecta:
  <https://simonwillison.net/2025/Jun/16/the-lethal-trifecta/>
- Landlock:
  <https://docs.kernel.org/userspace-api/landlock.html>
- AppContainer:
  <https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-for-legacy-applications->
- Apple containerization sandbox question:
  <https://github.com/apple/containerization/issues/737>

Python/runtime references:

- Jupyter messaging:
  <https://jupyter-client.readthedocs.io/en/latest/messaging.html>
- Python shared memory:
  <https://docs.python.org/3/library/multiprocessing.shared_memory.html>
- PEP 684:
  <https://peps.python.org/pep-0684/>
- IPython display behavior:
  <https://ipython.readthedocs.io/en/stable/api/generated/IPython.core.interactiveshell.html>
- RestrictedPython's own scope warning:
  <https://restrictedpython.readthedocs.io/>
