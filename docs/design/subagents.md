# Subagents

Status: design. Written 2026-08-27 against branch `agent-code-execution`.

What this covers: how Pi does subagents, how the React stack does them today, and
the design for the gpui (Rust) app. Section C is the proposal; A and B are the
evidence it argues from.

**Two answers up front**, because they are the questions the design turns on:

1. **The inspected thread is a real `agent_threads` row.** Today (React) it is
   *not* — the child transcript lives as JSON inside the parent's message rows —
   and that is the thing this design changes. See §C.2.
2. **Finished subagents stay listed and stay inspectable.** They leave the
   floating pill's count (which is live state) and remain as two chips in the
   transcript plus a row in the dialog, forever, because their thread is durable.
   Today they survive only until the session is disposed.

---

## A. How Pi does it

### A.0 Provenance

`@mariozechner/pi-*` no longer exists; the scope was renamed. Installed here:

| Path | Version |
|---|---|
| `/Users/julian/github/luma/node_modules/@earendil-works/pi-agent-core/package.json` | 0.80.2 |
| `/Users/julian/github/luma/node_modules/@earendil-works/pi-ai/package.json` | 0.80.2 |

`grep -ril subagent node_modules/@earendil-works/` returns **nothing**. The
subagent extension ships in no tarball — it lives only in the monorepo's
`examples/`. Source read from `earendil-works/pi@main`; paths below are repo
paths in *that* repo, and line numbers are that file's.

The core has no subagents by policy — `packages/coding-agent/docs/usage.md:309`:

> "It intentionally does not include built-in MCP, sub-agents, permission popups,
> plan mode, to-dos, or background bash. You can build or install those workflows
> as extensions or packages…"

Relevant files: `packages/coding-agent/examples/extensions/subagent/index.ts`
(1038 lines — the whole feature), `.../subagent/agents.ts` (157, discovery),
`.../subagent/agents/{scout,planner,reviewer,worker}.md`,
`packages/coding-agent/docs/extensions.md` (the extension contract).

### A.1 Tool schema

Registration and the verbatim description, `index.ts:471-481`:

```ts
pi.registerTool({
    name: "subagent",
    label: "Subagent",
    description: [
        "Delegate tasks to specialized subagents with isolated context.",
        "Modes: single (agent + task), parallel (tasks array), chain (sequential with {previous} placeholder).",
        `Default agent scope is "user" (from ${path.join(getAgentDir(), "agents")}).`,
        `To enable project-local agents in ${CONFIG_DIR_NAME}/agents, set agentScope: "both" (or "project").`,
    ].join(" "),
    parameters: SubagentParams,
```

Parameters, `index.ts:442-469` — every field optional; the mode is discriminated
at runtime, not by the schema:

- `agent: string` — "Name of the agent to invoke (for single mode)"
- `task: string` — "Task to delegate (for single mode)"
- `tasks: {agent, task, cwd?}[]` — "Array of {agent, task} for parallel execution"
- `chain: {agent, task, cwd?}[]` — task carries an optional `{previous}` placeholder
- `agentScope: "user" | "project" | "both"` (default `"user"`)
- `confirmProjectAgents: boolean` (default `true`)
- `cwd: string`

**The available agent names are not in the description.** The model discovers
them by guessing, or from the error path at `index.ts:507-518` and `:716-720`
(`"Invalid parameters. Available agents: ${available}"`), or from a workflow
prompt template that names them literally. `formatAgentList()`
(`agents.ts:149-157`) exists and is never called — dead helper. No
`promptSnippet` is set, so the tool contributes no line to the system prompt.

### A.2 Spawn: subprocess, not in-process

No `AgentSession` is constructed. It shells out to the `pi` CLI —
`index.ts:300-350`:

```ts
const args: string[] = ["--mode", "json", "-p", "--no-session"];
const inheritsDispatchConfig = !agent.model;
const model = agent.model ?? dispatchDefaults.model;
if (model) args.push("--model", model);
if (inheritsDispatchConfig && dispatchDefaults.thinkingLevel) {
    args.push("--thinking", dispatchDefaults.thinkingLevel);
}
if (agent.tools && agent.tools.length > 0) args.push("--tools", agent.tools.join(","));
```

```ts
if (agent.systemPrompt.trim()) {
    const tmp = await writePromptToTempFile(agent.name, agent.systemPrompt);
    args.push("--append-system-prompt", tmpPromptPath);
}
args.push(`Task: ${task}`);
const proc = spawn(invocation.command, invocation.args, {
    cwd: cwd ?? defaultCwd, shell: false, stdio: ["ignore", "pipe", "pipe"],
});
```

- **System prompt**: the agent `.md` body is written to a `0600` temp file
  (`index.ts:239-247`) and passed as `--append-system-prompt`. It *appends* to
  pi's default coding-agent prompt; `--system-prompt` would replace. Unlinked in
  the `finally` at `:426-438`.
- **Model**: frontmatter `model:` wins, else the dispatching session's model
  (`index.ts:485-488`). Thinking level is inherited only when the agent did not
  pin a model.
- **Tools**: `--tools` is an allowlist over built-in, extension and custom tools,
  applied only when frontmatter declares `tools:`.
- **No `--no-extensions`**, no env scrubbing: the child loads the same extensions
  and inherits the environment.
- Which binary, `index.ts:249-263`: prefers `process.execPath + argv[1]`, falls
  back to `execPath` alone, then literally `"pi"` on `PATH`.

Discovery (`agents.ts:116-147`) reads `~/.pi/agent/agents` and, walking up from
`ctx.cwd`, `<CONFIG_DIR>/agents`. Project entries override user entries by name.
Frontmatter via pi's exported `parseFrontmatter`. **Discovery re-runs on every
tool call**, so agent files can be edited mid-session.

### A.3 Progress streaming

Two hops.

**Child → extension**, over stdout JSON lines. `--mode json` emits one event per
line; the extension line-buffers (`index.ts:390-395`) and reacts to two event
types (`index.ts:353-387`): `message_end` (accumulating `messages`, turn count,
input/output/cacheRead/cacheWrite tokens, cost, `stopReason`, `model`) and
`tool_result_end`. **`tool_result_end` is a dead branch** — no such event exists
in current pi (absent from `docs/json.md:37-51`, from `agent-session.ts`, and
from the installed `pi-agent-core/dist/types.d.ts`); tool results already arrive
as `message_end` with `role: "toolResult"`. Streaming is therefore **per-message,
not per-token**: `message_update` deltas are ignored entirely.

**Extension → parent UI**, via `onUpdate`, `execute`'s 4th argument
(`index.ts:324-331`):

```ts
const emitUpdate = () => {
    if (onUpdate) {
        onUpdate({
            content: [{ type: "text", text: getFinalOutput(currentResult.messages) || "(running...)" }],
            details: makeDetails([currentResult]),
        });
    }
};
```

The loop turns that into a `tool_execution_update` event
(`node_modules/@earendil-works/pi-agent-core/dist/agent-loop.js:420-430`), which
any extension can observe and which the TUI uses to re-invoke
`renderResult(..., { isPartial: true })`. It does render live: parallel results
carry `exitCode: -1` as a "still running" sentinel (`index.ts:625`, `:946`,
`:1015`), showing `⏳` per task and a `2/3 done, 1 running` header (`:955-957`).

### A.4 Tool result

`content` — what the *model* sees — is the child's **final assistant text only**.
`getFinalOutput` scans backwards for the last assistant message and returns its
first text part (`index.ts:170-180`). Per mode: single returns it directly
(`:710-713`); chain returns only the **last** step's output (`:598-601`),
intermediate steps being passed forward by `{previous}` substitution (`:556`);
parallel returns a concatenated markdown digest, each task capped at
`PER_TASK_OUTPUT_CAP = 50 * 1024` bytes (`:36`, `:193-202`, `:669-685`).

`details` — UI-only, never sent to the model — carries every `SingleResult`
including all child messages, `stderr`, `exitCode`, `stopReason`, `model` and
`UsageStats` (`:139-168`).

**Token usage is tracked but not propagated.** The extension does not return the
documented `usage` field (`docs/extensions.md:2013`), so child cost is invisible
to the parent session's accounting — display-only in `renderResult`.

### A.4b Where the child's transcript lives

**The child persists nothing.** The word `session` appears exactly once in the
extension's 1038 lines — `index.ts:300`, the spawn arguments:

```ts
const args: string[] = ["--mode", "json", "-p", "--no-session"];
```

`--no-session` is documented as **"Ephemeral mode; do not save"**
(`packages/coding-agent/docs/usage.md:83, 205`; `docs/sessions.md:12`). Normally
"Sessions are saved automatically to `~/.pi/agent/sessions/`, organized by
working directory. Each session is a JSONL file with a tree structure"
(`docs/sessions.md:7`, `usage.md:78`). So the child writes no file, and its
process exit is the end of it.

**A variant that keeps one is available and unused.** The CLI already offers
`--session <path|id>` and `--session-dir <dir>` (`usage.md:202-205`), either of
which would give each child its own file. The extension passes neither.

**The parent stores the whole child transcript anyway — as a blob.** The tool
result's `details` field carries `results: SingleResult[]`, and each
`SingleResult` includes `messages: Message[]` — every child message
(`index.ts:126-137, 139-145`). `details` is a first-class field on
`ToolResultMessage`
(`node_modules/@earendil-works/pi-ai/dist/types.d.ts:284-292`), and the harness
appends that message object verbatim
(`pi-agent-core/dist/harness/agent-harness.js:352-364` →
`harness/session/session.js:97-105`, `appendMessage` storing `message` whole).
It is never stripped before the write. So:

- **to the model**: only the final assistant text (§A.4), clamped;
- **to the parent's session file**: every child message, nested inside one
  tool-result entry's `details`;
- **to the child's own storage**: nothing.

**Inspection afterwards** is therefore whatever renders `details`. Live, that is
`renderResult` in the TUI. Later, it is reading the parent's JSONL by hand —
there is no `pi sessions` view of a child, no child session id, and nothing to
resume. Reconstructing a child means parsing the parent's file, which is exactly
the shape §B.1 describes on the Luma side.

**Core session model: parent/child linkage exists, and the extension does not use
it.** `JsonlSessionMetadata.parentSessionPath` (`harness/types.d.ts:307-311`,
`:345`) is written to the JSONL header as `parentSession`
(`harness/session/jsonl-storage.js:106, 160`), but it is set only by `fork()`
(`harness/session/jsonl-repo.js:85`, `options.parentSessionPath ??
sourceMetadata.path`) — session *branching*, the same feature
`SessionTreeEntryBase.parentId` (`types.d.ts:230-235`) serves within one file.
Neither is a subagent relation, and the extension touches neither.

Net: Pi has the two ingredients for a durable child thread — per-session files
and a `parentSessionPath` header — and the subagent extension wires up neither.
This is the strongest single argument for §C.2's Design 2: both reference
implementations independently chose the transcript-in-a-blob shape, and both did
so by *omission* rather than by a reason either states.

### A.5 Cancellation

`AbortSignal` is `execute`'s 3rd parameter, threaded to `runSingleAgent`.
`index.ts:410-420`: SIGTERM on abort, SIGKILL 5s later. On close,
`if (wasAborted) throw new Error("Subagent was aborted")` (`:423-424`) — throwing
is the only way to set `isError: true` (`docs/extensions.md:2015`). Chain mode
propagates immediately; parallel mode kills every spawned child because each
registered its own listener on the same signal. Two small leaks on this path: the
SIGKILL `setTimeout` is never cleared, and placeholders for never-started
parallel tasks are discarded.

### A.6 Nesting: allowed, unguarded

No depth counter, no env marker, no exclusion of `subagent` from the child's tool
list, no `--no-extensions`. The child auto-discovers the same extension.
`scout`/`planner`/`reviewer` declare explicit `tools:` frontmatter that happens to
omit `subagent`, so nesting is blocked *incidentally*. `worker.md` declares no
`tools:`, so the guard at `index.ts:307` is false, no `--tools` is passed, and a
worker gets the full set **including `subagent`** — unbounded recursion, each
level a fresh OS process with a fresh context window.

### A.7 Concurrency

Within one call (`index.ts:33-34`): `MAX_PARALLEL_TASKS = 8` (rejected with a
text result, not an error, at `:605-614`) and `MAX_CONCURRENCY = 4`, enforced by a
hand-rolled worker pool (`:219-237`). Chain mode is sequential and stops at the
first failure (`:587-595`).

Across calls: **no limit**. Pi runs a batch's tool calls in parallel by default,
so four `subagent` calls of eight tasks each is sixteen concurrent processes with
sixteen model connections and no shared semaphore.

### A.8 The extension mechanism

No `defineExtension`. An extension is a TS module with a default-exported factory
taking `ExtensionAPI`, loaded through jiti with no compile step
(`docs/extensions.md:156-181`), discovered from `~/.pi/agent/extensions/*.ts`,
`.../<name>/index.ts`, and the project-local `.pi/extensions/` equivalents after
project trust.

**The subagent extension uses exactly one API: `pi.registerTool()`.** No event
handlers, no commands, no shortcuts, no flags. Its entire lifecycle is inside
`execute`. The `/implement` workflows are not extension commands — they are plain
markdown prompt templates that tell the model to call the tool.

From `ctx: ExtensionContext` it uses `cwd`, `model`, `thinkingLevel`, `hasUI`,
`isProjectTrusted()`, `ui.confirm()`. The forty-odd lifecycle hooks pi offers
(`docs/extensions.md:273-958`) are all unused.

### A.9 A gate worth flagging

`index.ts:520-548` confirms before running project-local agents only when
`(agentScope === "project" || "both") && confirmProjectAgents && ctx.hasUI &&
!ctx.isProjectTrusted()`. `agentScope` and `confirmProjectAgents` are both
**model-supplied**, so a model can disable its own confirmation; and `ctx.hasUI`
is false inside any subagent (which runs `-p --mode json`), so a nested call
never prompts. The gate holds only when the parent model cooperates. Do not port
this shape.

---

## B. How the React version does it

### B.1 Thread identity — no child thread row

**There is no child `agent_threads` row and no `parent_thread_id`.**

- `src-tauri/migrations/20260728000000_agent_threads.sql:25-35` — columns are
  `id, agent_kind, subject_kind, subject_id, venue_id, score_id, title,
  created_at, updated_at`.
- Later ALTERs add only `owner_user_id`
  (`20260801000000_agent_thread_owners.sql:4`), `lifecycle_state`
  (`20260801200000_agent_thread_lifecycle.sql:7`), `implementation_id`
  (`20260801300000_agent_thread_graph_implementation.sql:16`), `actor`
  (`20260827000000_authored_revision_actor.sql:18`), and
  `forked_from_thread_id` / `forked_at_message_id`
  (`20260802945000_relational_authored_history.sql:398-399` — time-travel restore
  forking, not subagents).
- `parent_message_id` exists, but on `agent_thread_messages`
  (`20260802945000_relational_authored_history.sql:427,433-436,476`) — the
  message-lineage DAG for restore, unrelated.

**The child transcript lives inside the parent thread's messages**, two ways:

1. Live, in memory: `SubagentManager.records[].messages`
   (`src/shared/lib/agent/subagents/subagent-manager.ts:186`, updated in
   `onMessages` at `:344-348`). Child messages use the same `AgentChatMessage[]`
   shape as the parent, so the same renderer works.
2. Persisted, as JSON in the **parent**'s `agent_thread_messages.parts_json`:
   embedded in the `Agent` tool call's output as
   `AgentToolOutput.subagent: SubagentSnapshot`
   (`src/shared/lib/agent/subagents/types.ts:142-148`, produced at
   `tools.ts:112,123-128,155`), and separately as `data-subagent` milestone rows
   (§B.2).

Reload is a **parser over persisted JSON**: `subagentStatesFromMessages`
(`src/shared/components/agent-chat/subagent-state.ts:139-165`) walks parent
parts, picks up both `part.type === "data-subagent"` and
`toolPart.output.subagent`, and recurses into `candidate.messages` for
grandchildren with a cycle guard. `mergeSubagentStates` (`:170-190`) overlays
live manager state and rewrites a persisted `running` snapshot with no live twin
to `status: "aborted", error: "Subagent session ended."`.

Inspection: the side panel renders `subagent.messages` through the ordinary
`AgentConversation` with `showUserMessages={false}`
(`src/shared/components/agent-chat/subagent-panel.tsx:175-181`).

### B.2 Finished subagents

**They stay in the manager forever, for the session.** Nothing deletes from
`this.records`; there is no eviction in `subagent-manager.ts`. `list()` returns
terminal records too (`:216-218`). They die only on `dispose()` (`:264-278`),
called from `create-agent-chat.ts:1025` on session switch/teardown.

Terminal transition at `subagent-manager.ts:391-399`: sets `finishedAt`,
`phase = "terminal"`, `status`, `result` or `error`, emits `{type: "end", ...}`.

`flushSubagentSnapshots` — `src/shared/components/agent-chat/create-agent-chat.ts:587-620`:

```ts
const snapshots = [...session.pendingSubagentSnapshots.values()].map(
    (snapshot) => structuredClone(snapshot),
);
session.pendingSubagentSnapshots.clear();
const milestones: AgentChatMessage[] = snapshots.map((snapshot) => ({
    id: `subagent:${snapshot.id}:${snapshot.status}`,
    role: "assistant",
    parts: [{ type: "data-subagent", data: snapshot }],
}));
```

"Milestones" are synthetic **assistant** rows whose only part is `data-subagent`.
The `subagent:<id>:<status>` id makes the write idempotent per (child, terminal
status). Gated on `activeTurns.size === 0 && pendingFinalization === null`
(`:592-598`), serialized on a `flushingSubagents` tail, rolled back on persist
failure (`:612-618`). Only terminal snapshots are queued (`:952-959`) — running
ones are never persisted by this path. Callers: `:961`, `:1037`, `:1380`,
`:1448`. They are excluded from the model's history (`messages.ts:430`) and
skipped when locating a turn's assistant message (`messages.ts:187`,
`create-agent-chat.ts:1359`).

**These milestone rows are the 1811 violation** named in
`project_authored_turn_invariant`: assistant rows nothing ever prepares.

**Does the parent tool result carry the child's output?** Yes. Foreground `Agent`
returns `{agent_id, status, result, subagent}`; `toModelOutput`
(`tools.ts:134-142`) sends the model only
`clampForModel(result, 16_000, {label: "subagent result"})`, or
`JSON.stringify({agent_id})` when backgrounded. The `subagent` key is stripped
from the UI's generic tool-detail JSON (`conversation.tsx:628-636`).

**What the chip shows on finish**: only the avatar and the `description`
argument; the row's trailing text flips to `"finished working"`. No summary, no
result, no duration. The *panel* row shows
`lastSubagentText(subagent) ?? subagent.error ?? subagentAction(subagent)`
(`subagent-panel.tsx:63-64`).

### B.3 Manager design — bespoke, not Pi's

`pi-agent-core` has zero `subagent` references. `git log` shows the manager
landed in `94c8727a subagents: authored-workspace subagents with supervisor and
chat UI`, **before** `ea8e85b3 replace agent loop with Pi`. Pi supplies only the
single-agent loop.

Diff against Pi:

| | Pi extension | Luma React |
|---|---|---|
| Child runtime | OS subprocess (`pi --mode json -p`) | **in-process**, same JS thread; fresh `createPiAgent` per run (`pi-runner.ts:25-32`) |
| System prompt | `--append-system-prompt` temp file | `AgentLoader.buildSystemPrompt` (`agent-loader.ts:98-124`), frontmatter `prompt_mode: append \| replace` |
| Tools | `--tools` allowlist, else everything | parent-equivalent domain tools **rebound to the child workspace** (`create-agent-chat.ts:881-893`), filtered by `config.toolNames` (`subagent-manager.ts:177-186`, `:280-287`) |
| Isolation | cwd only | an authored workspace (§B.5) — Pi has no analogue |
| Nesting | unguarded, incidental | **explicit**: child gets `Agent`/`get_subagent_result`/`steer_subagent` with `parentSubagentId` (`subagent-manager.ts:172-177`). Unbounded, no depth limit |
| Concurrency | 8 tasks / 4 workers per call, no global cap | **no cap at all** — grep for `maxConcurren\|concurrency\|limit\|depth` in the subagents dir returns nothing |
| Progress | stdout JSON → `onUpdate` → `tool_execution_update` | push store: `subscribe(listener)` + `notify()` (`:220-224`, `:415-424`), read by React at `agent-chat-panel.tsx:111-116`. An `onEvent` callback exists in `types.ts:57-69` and is never supplied |
| Result to model | last assistant text | same, clamped to 16k, plus an `<authored_merge>` tag (§B.5) |
| Steering | none | `steer()` (`:230-244`), drained at `pi-runner.ts:49-54` |
| Cancellation | SIGTERM → SIGKILL | per-record `AbortController`; three phases `running → finalizing → terminal`; two modes — `"detach"` (parent stops waiting, child runs on) and `"abort-before-finalization"` (`:295-329`), so a committed workspace can never be reported aborted |

Model selection: `options.model ?? config.model` (`subagent-manager.ts:207`),
validated against `availableModels` (`tools.ts:92-97`). The injected environment
string (`create-agent-chat.ts:857-858`) is:

> `Application: Luma desktop\nAuthored state: isolated child revision\nEdits: parent-equivalent tools target only this revision; the supervisor merges it after completion`

### B.4 Chip visuals — exact

`AgentChip`, `src/shared/components/agent-chat/conversation.tsx:291-321`:

```tsx
<button
  type="button"
  onClick={() => navigate?.(subagent.id)}
  disabled={!navigate}
  className={cn(
    "inline-flex max-w-64 items-center gap-1.5 rounded-full border border-border px-2 py-0.5",
    failed ? "text-destructive" : VERB,
    navigate && "hover:bg-control hover:text-foreground",
  )}
>
  <SubagentAvatar seed={subagent.id} className="size-4" />
  <span className="truncate">{description}</span>
</button>
```

- `VERB = "text-muted-foreground"` (`:277`), `DETAIL = "text-muted-foreground/75"`
  (`:278`), `SHIMMER = "agent-shimmer"` (`:279`).
- Geometry: `max-w-64` = 256px, `gap-1.5` = 6px, `px-2` = 8px, `py-0.5` = 2px,
  avatar `size-4` = 16px, fully rounded, 1px `border-border`.
- `description` = `tool.input.description` (the model's 3–5 word label), else
  `subagent.type`, else `"Agent"` (`:303-307`).
- States: `failed = tool.state === "error" || subagent.status === "error"`
  (`:308`) → `text-destructive`. **`aborted` is not visually distinguished.**
- **No spinner, no per-chip shimmer** — the chip itself is static.
- Click → `useSubagentNav()` (`subagent-nav.ts:3-9`), opening the side panel on
  that child. Disabled with no nav context.
- Avatar: DiceBear `identicon`, `scale: 70`, seeded by subagent id
  (`subagent-avatar.tsx:14-25`).

`AgentChipsRow`, `conversation.tsx:323-345` — consecutive `Agent` tool parts are
coalesced into one row by `coalesceAgents` (`:580-599`):

```tsx
<div className={cn("flex flex-wrap items-center gap-x-2 gap-y-1.5", DETAIL)}>
  {items.map(...)}
  <span>
    {items.every(({ subagent }) => isSubagentDone(subagent))
      ? "finished working"
      : "started working"}
  </span>
</div>
```

The literal strings are **"started working"** and **"finished working"** — there
is no "Agent started" string anywhere in the repo. The only shimmer in the chip
path is the collapsed-group header `<strong className={cn("font-medium", VERB,
SHIMMER)}>Working</strong>` (`conversation.tsx:513-520`); shimmer CSS is
`src/App.css:11-40` (1s linear infinite, `background-clip: text`, disabled under
`prefers-reduced-motion`).

No screenshot was taken: the chip is a static bordered pill with an identicon and
one line of truncated text, and the class strings above pin every value a
screenshot would have measured. Running Vite to photograph it would add nothing
the port needs.

### B.5 The side panel (being dropped) and what it uniquely gave

`SubagentsPane`, `subagent-panel.tsx:188-255`, mounted as a right overlay
(`agent-chat-panel.tsx:397-419`,
`"absolute inset-y-0 right-0 z-30 w-[min(24rem,calc(100%-2rem))] border-l border-border bg-background shadow-2xl"`).
Two screens under a 40px header: a **list** (`SubagentList`, `:96-144` — Active
section, then `Done · N` newest-first, paginated 10 at a time) and a **drill-in**
(`SubagentFeed`, `:146-184` — back arrow, avatar, title, and the child's full
transcript via `AgentConversation`, auto-scrolled with a 40px stick threshold).

Unique to the panel: (a) the child's full transcript with nested grandchild
chips; (b) an Active/Done inventory including children whose tool call has
scrolled away; (c) last-activity relative timestamps; (d) live one-line status
("using X…", `subagentAction()` at `subagent-state.ts:121-134`); (e) inline error
text; (f) an unread badge (`agent-chat-panel.tsx:293-313`) and auto-open at
≥720px (`:153-165`).

The gpui design keeps (a), (b), (d) and (e) in the dialog, and replaces (f) with
the floating pill. (c) is cheap to keep and worth keeping.

### B.6 Workspaces

Allocation — `AuthoredSubagentSupervisor.prepare`
(`src/shared/lib/agent/subagents/authored-subagent-supervisor.ts:178-506`),
called from `prepareSpawn` (`create-agent-chat.ts:858-921`) **before** the child
starts:

1. `checkpointForSubagent()` (`:807-820`) commits the parent editor's live state.
2. `mergeScheduler.reserve(runId, parentSubagentId)` (`:182`) takes a
   deterministic **post-order** merge slot (`:108-156`): siblings keep spawn
   order via `rootTail`; a nested run's predecessor is its parent's
   `childrenTail`, so a foreground parent awaiting a child cannot deadlock.
3. Root child → `currentAuthoredRevision(threadId)` then
   `createAuthoredWorkspace({threadId, requestId, expectedBaseRevisionId})`
   (`:215-221`). Nested child → parent's `checkpoint()` then
   `forkAuthoredWorkspace({sourceWorkspaceId})` (`:197-211`).
4. Domain tools are rebuilt against
   `{subagentId, workspaceId, initialDocument, bindWorkspaceDocument}`
   (`create-agent-chat.ts:886-892`), guarded by `assertSameDomainToolSurface`
   (`:894`) and wrapped in `serializeWorkspaceToolExecution` (`:895-898`) so one
   workspace has at most one operation in flight.

`finalize(summary)` (`authored-subagent-supervisor.ts:442-457` → `runFinalize`
`:325-440`) runs from the manager's `prepared.finalize` hook
(`subagent-manager.ts:375-380`) **only on `status === "completed"`**
(`create-agent-chat.ts:903`): wait for the merge slot → editor checkpoint →
`checkAuthoredWorkspace` → if unchanged, retire → else `commitAuthoredWorkspace`
with `message: subagentRevisionSubject(result)` (first non-empty line, collapsed,
240 UTF-8 bytes with `…`, `:56-79`) → `mergeAuthoredWorkspace` (root) or
`mergeAuthoredWorkspaceIntoWorkspace` inside the parent's serialized queue,
retried once (`:378-409`) → on conflict return
`{status: "conflicted", proposalRevisionId, conflicts}` and retire the workspace
anyway, the immutable proposal revision being the whole handoff (`:418-420`).

The model sees the merge outcome appended to the child's text —
`withAuthoredMergeResult` (`create-agent-chat.ts:344-352`):

```ts
`${childResult}\n\n<authored_merge status="${finalization.status}" revision_id="${finalization.revisionId}"/>`
```

Rust side, `src-tauri/src/services/authored_documents/workspaces.rs:940-1078`:
`merge_workspace` locks the thread, verifies `head_revision_id ==
expected_head_revision_id`, replays a prior outcome by operation id, refuses a
dirty directory (`"workspace has uncommitted changes; commit it before merging"`,
`:990-994`), fast-forwards when the child head is already an ancestor, else
3-way `merge_snapshots(base, ours = main, theirs = child)`, then
`apply_candidate_locked(..., parents = [main.head, child.head])`.
`merge_workspace_into_workspace` (`:1091+`) is the nested variant.

Failure: `prepared.cleanup` always runs (`subagent-manager.ts:382-390`) and calls
`workspace.discard()` (`create-agent-chat.ts:915-918`) — after error or abort it
is the *only* path, so the child's revisions are thrown away, never merged.
`discard()` (`:459-485`) still waits for its merge slot so sibling ordering
survives. If cleanup throws on a completed outcome, the outcome is downgraded to
`error` (`subagent-manager.ts:386-389`). `remove_workspace`
(`workspaces.rs:1435-1451`) retires the row and deletes the directory; thread
deletion sweeps leftovers.

---

## C. Design for gpui

### C.1 The UI contract

Three surfaces, and only three:

1. **One chip in the transcript, read in three tenses.** A delegation is *one*
   tool call — the loop never fabricates a finish call — so it lands as a single
   entry on the existing rail at the point in the conversation where it
   happened, and its trailing line says which of "started working" / "finished
   working" / "failed" it is at. That reading is taken off the stored output,
   not off `ToolState`: a child that ran and failed still answers its call
   successfully, because losing the child's thread id would lose the only record
   of what it did (`Delegation` / `delegation()`,
   `gpui/crates/chat/src/chip.rs:299-343`). The row keeps the rail's rhythm and
   `CHIP_HEIGHT = 38.0` (`gpui/crates/chat/src/theme.rs`) but spends its width on
   an identicon and the model's own label instead of a tile and a chevron
   (`subagent_pill`, `chip.rs:356-434`); the verb table entry
   (`verb()`, `chip.rs:72-76`) stays so the automation tree and the pill's line
   cannot drift.
2. **A floating pill above the composer** — "3 subagents working". Live state,
   not transcript: it is the answer to "is anything running right now", and it
   disappears when the count is zero. It mounts exactly where the context card
   does, `float::anchored_above` (`gpui/crates/ui/src/float.rs:635`, used by
   `usage::open_card` at `gpui/crates/chat/src/usage.rs:94-96`), inside the
   footer's `relative` block (`gpui/crates/chat/src/lib.rs:1026-1064`).
3. **One dialog with two routes.** Click the pill → route `List`: every subagent
   of this thread, running and finished. Click a row → the dialog *morphs* into
   route `Thread(id)`: that child's transcript, read-only, scrollable, with a
   back arrow. This is `MorphDialog<Route>`
   (`gpui/crates/ui/src/dialog/morph.rs:533`) exactly as `AddTracks` uses it
   (`gpui/crates/app/src/add_tracks.rs:113-123, 227, 400`), including
   `Route::parent()` for back.

What is *not* built: a side panel, an unread badge, auto-open, a second
transcript renderer. Julian's phrasing "Agent started pill" refers to what React
renders as `"started working"` / `"finished working"` (§B.4) — that phrasing
ports as chip verbs (`"Started"` / `"Finished"` + noun `"subagent"`), which is
how every other chip in the gpui panel narrates.

### C.2 Two designs, and the choice

**Design 1 — child transcript embedded in a parent part.** The React shape:
snapshots inside `AgentToolOutput.subagent` plus `data-subagent` milestone rows,
reconstructed on load by a parser.

**Design 2 — child as a real thread row.** A subagent gets an `agent_threads`
row with `parent_thread_id` and `parent_call_id`; its messages are ordinary
`agent_thread_messages`; the dialog opens it with the existing
`AgentChat::open_thread` path.

**Pick Design 2.** Reasons, in order of weight:

1. **It defines the 1811 abort out of existence.** Design 1's milestone rows are
   assistant rows nothing prepares — the violation
   `project_authored_turn_invariant` records, still live today. Design 2 has no
   milestone rows: a child's assistant rows live in the child thread and the Rust
   loop already prepares exactly one authored turn per assistant row
   (`src-tauri/src/agent/turn.rs:6-11, 311-330`), which is precisely the
   discipline the trigger asks for. See §C.6 for the one thing this needs.
2. **It removes a parser.** Design 1 needs `subagentStatesFromMessages`' walk,
   its recursion into grandchildren and its cycle guard
   (`subagent-state.ts:139-165`) — a second, weaker representation of something
   `agent_thread_messages` already is. "Different layer → different abstraction";
   here it is the same abstraction twice.
3. **The dialog's read-only thread is free.** `Transcript` and
   `luma_lib::agent::apply` are the canonical reducer both hosts already use
   (`src-tauri/src/agent/transcript.rs:509`), `open_thread` already exists
   (`gpui/crates/chat/src/lib.rs:107-120`), and `transcript::row` already renders
   a virtualized list. Design 1 forces a parallel renderer over snapshot JSON.
4. **Row size.** Design 1 rewrites the parent's `parts_json` with the child's
   entire growing transcript on every snapshot, and those rows are what syncs.
   Design 2 appends one child row per child message.
5. **Inspection after the fact.** A durable child thread is greppable by the
   existing history search (`AgentService::history`) and survives a restart. A
   snapshot blob is legible only to the code that wrote it.

Cost of Design 2: two columns, one migration, and the workspace-scoped
`prepare_turn` in §C.6. That is the honest price of not having two transcript
stores.

### C.3 The tool surface

Mirror Pi's extension *shape* — one tool, registered in the same registry, with
no host handle — not its subprocess mechanics. The Rust side is already built for
this and says so: `src-tauri/src/agent/tools/mod.rs:1-7` opens with

> "A tool is bound to a *context*, never to a host. That is what makes a
> subagent's tool set identical to its parent's by construction rather than by
> assertion: the same `registry` call builds both, and only the `ToolContext`
> differs (a child execution namespace, a detached authored workspace). There is
> no second builder that could drift."

and `ToolContext` already carries `execution_id: Option<&str>` and
`authored_workspace_id: Option<&str>` (`:48-54`), `AgentService::with_tools`
exists as "the one seam a subagent needs" (`agent/mod.rs:575-581`), `TurnEvent`
already has a `Subagent { snapshot: Value }` variant documented as never
persisted (`agent/mod.rs:494-497`), the reducer already drops it
(`transcript.rs:592-594`), and `resolve_execution_id`
(`src-tauri/src/services/agent_execution.rs:87-104`) already enforces the
invariant **a child's Python execution id equals its authored workspace id**.
Almost none of the plumbing is new; what is missing is the tool and the
supervisor.

Add `src-tauri/src/agent/tools/subagent.rs`, one tool, three arguments:

- `agent: String` — the named agent from the loader.
- `task: String` — the delegated instruction.
- `description: String` — 3–5 words, present tense. This is the chip's title, the
  dialog row's name, and the only text a reader sees at a glance; it is a
  required argument for the same reason `python`'s `purpose` is (`chip.rs:105`).

Deliberately **not** ported from Pi: `tasks`/`chain` modes (the model can emit
parallel tool calls; a chain is two turns), `cwd` (there is no cwd — a child edits
a workspace), and `agentScope`/`confirmProjectAgents` (§A.9 is a
model-disables-its-own-gate hole; agents are bundled, not repo-supplied).

Result to the model: the child's final assistant text, clamped through the
existing `clamp_for_model` (`tools/mod.rs:143`), plus the merge outcome as
React's `<authored_merge …/>` tag. The child's transcript is **not** in the
result — it is a thread id away.

Because a `finish` must also be a chip (§C.1), the finish is emitted as its own
tool call rather than as the start call's output: the start chip is the call the
model made, the finish chip is the supervisor's completion record. Both are
ordinary `AgentChatPart::Tool` parts, so both persist and re-render with no new
part type.

### C.4 What is live and what is persisted

| Fact | Where |
|---|---|
| Child transcript | **persisted** — the child thread's own rows |
| Start / finish chips | **persisted** — ordinary tool parts in the parent |
| Merge outcome | **persisted** — inside the finish chip's output |
| "3 subagents working" | **live** — `TurnEvent::Subagent { snapshot }`, never persisted (`agent/mod.rs:494-496`) |
| "using python…" one-liner | **live** — same snapshot |
| Elapsed time | **live**, derived from `started_at` on the row |

No milestone rows. The invariant to hold: **anything durable is already a row
somewhere; the snapshot stream carries only what is true right now.** That is the
existing comment's rule, honoured rather than worked around.

The gpui host folds snapshots in `AgentChat::on_event`
(`gpui/crates/chat/src/lib.rs:818-844`), which already ignores events whose
`Applied.row` is `None` — so a snapshot costs no remeasure. It lands in a new
`subagents: Vec<SubagentSnapshot>` field beside `transcript`, which is what both
the pill and the dialog list read.

### C.5 Concurrency, nesting, cancellation

Pi caps fan-out per call and nothing globally (§A.7); React caps nothing at all
(§B.3). Neither is defensible when each child holds a workspace directory, a
Python kernel and a model connection. Set both limits in one place, on the
supervisor:

- **max concurrent children per thread: 4**, matching Pi's `MAX_CONCURRENCY`. A
  fifth start returns a tool *error* naming the limit, not a queue — a queued
  subagent is a turn that appears hung.
- **max depth: 2** (a child may spawn, a grandchild may not). Enforced by *not
  putting the subagent tool in a depth-2 registry* — the registry is already the
  place tool surfaces are decided (`tools/mod.rs:127-141`), so this is a match arm,
  not a runtime check. This is Pi's incidental block made deliberate.

Cancellation: keep React's three phases and two abort modes
(`subagent-manager.ts:295-329`) — the distinction between "stop waiting" and
"kill it" is real, and the rule that an abort after finalization begins is
ignored is what stops a committed workspace being reported as aborted. In Rust
this is a `CancellationToken` per child plus a `phase` field; the existing
`TurnStream`-drop cancellation (`chat/src/lib.rs:73-79`) already gives the parent
side for free.

### C.6 The authored-state problem (the real work)

`prepare_turn` resolves the document **from the thread's scope** and locks the
live head — `src-tauri/src/services/authored_documents/turns.rs:26-33`:

```rust
let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, &input.thread_id).await?;
let mut write = self.scope_write(pool, &scope).await?;
let main = self.ensure_current_on_connection(connection, &scope).await?;
```

A child thread sharing the parent's scope would therefore prepare and finalize
**onto the live document**, contending with the parent's lock and destroying the
workspace isolation the whole feature exists for. This is the one place Design 2
costs something, and it must be fixed before a child thread ever writes an
assistant row.

The fix that keeps one canonical way: give `authored_turn_preparations` a
nullable `workspace_id`, and let `prepare_turn`/`finalize_turn` branch on the
thread's `authored_workspace_id` — a workspace-scoped thread prepares against the
**private workspace head** and finalizes through `commit_workspace`
(`workspaces.rs:792`), which already makes one-parent revisions and CASes the
private head under an operation id. The live head moves once, at
`merge_workspace` (`:940`), exactly as it does today. The trigger keeps its
current shape and its current strength: every assistant row still needs a
preparation, and now child rows have one.

The alternative — exempting workspace-scoped threads in the trigger — is cheaper
and worse: it weakens the invariant for a whole class of rows to avoid teaching
one function about workspaces.

Also note `merge_workspace_into_workspace` (`workspaces.rs:1091`) already exists,
so the depth-2 nested case is supported on the Rust side today; and the merge
ordering React solved with `AuthoredSubagentMergeScheduler`
(`authored-subagent-supervisor.ts:108-156`) must move to Rust intact — post-order
reservation is not an optimisation, it is what stops a foreground parent
deadlocking on its own child.

### C.7 Migration

| # | Change | Where | Size / status |
|---|---|---|---|
| 1 | `parent_thread_id`, `parent_call_id`, `authored_workspace_id`, `started_at` on `agent_threads`; index on `parent_thread_id` | new migration | ~30 lines |
| 2 | `workspace_id` on `authored_turn_preparations`; `prepare_turn`/`finalize_turn` branch on it | `services/authored_documents/turns.rs` | ~150 lines + migration |
| 3 | Supervisor: allocate/fork workspace, post-order merge scheduler, finalize, discard | new `src-tauri/src/agent/subagent.rs` | ~450 lines — the bulk |
| 4 | `subagent` tool (start), finish-call emission, depth-gated registry arm | `agent/tools/subagent.rs`, `tools/mod.rs:127` | ~180 lines |
| 5 | Agent loader port (frontmatter + bundled agents), sharing the existing frontmatter parser | `agent/skills.rs` neighbours | ~120 lines — check `docs/design/skills.md:44`, the parser is already shared |
| 6 | `SubagentSnapshot` type + emit `TurnEvent::Subagent` from the supervisor | `agent/mod.rs`, `models/` | ~80 lines |
| 7 | Chip verb + the identicon pill for `subagent` | `gpui/crates/chat/src/chip.rs:72-76` (verb), `:114` (detail), `:299-343` (`Delegation`), `:356-434` (`subagent_pill`), `:444-446` (`row` routes to it) | **done** |
| 8 | Floating pill | `gpui/crates/chat/src/subagents.rs` (`avatar`, `pill`), mounted at `gpui/crates/chat/src/lib.rs:1172` | **done** |
| 9 | Two-route morph dialog (list + read-only thread) | `gpui/crates/app/src/subagents.rs`, `Overlay::Subagents` (`gpui/crates/app/src/shell.rs:84, 97, 562-572, 987`), keymap context `SUBAGENTS` (`gpui/crates/app/src/keymap.rs:64`) | **done** |
| 10 | `subagents: Vec<SubagentSnapshot>` in `AgentChat`, folded in `on_event` | `gpui/crates/chat/src/lib.rs:294, 361, 404-406, 470, 895-903` | **done** |
| 11 | Delete the React side panel and manager once the gpui path is the only one | `src/shared/components/agent-chat/subagent-*.tsx` and `src/shared/lib/agent/subagents/**` are gone; the last of their plumbing (`executionId` / `authoredWorkspaceId` on the TS python and graph tools) went with them | **done** |

Two places the build deviates from the plan above, both deliberate:

- **No finish call.** Item 4 said "finish-call emission"; a delegation emits one
  tool call whose *output* carries the outcome. A second synthetic call would be
  a transcript row nothing wrote, and the reading it would carry already fits in
  the first row's output (§C.1 item 1).
- **Depth is checked, not arranged.** Item 4 said "depth-gated registry arm".
  The landed check walks the child's own ancestry against `MAX_DEPTH`
  (`src-tauri/src/agent/subagent.rs:42-46, 265, 411-434`), because
  `AgentService::with_tools` can hand any surface to any turn — a registry that
  omits the subagent tool is a convention, while the thread's parent chain is a
  fact.

Rough total: **~1600 lines added in Rust, ~2500 deleted in TS**, over 11 units,
of which items 2 and 3 carry the risk and the rest are mechanical.

### C.8 Risks

- **The 1811 trigger.** Named above. If item 2 is skipped and child threads write
  assistant rows anyway, every child turn aborts with
  `assistant message requires a prepared authored turn`. If item 2 is done
  half-way — preparing against the live head — children silently corrupt the
  parent's document instead. This is the one ordering constraint in the plan:
  **item 2 lands before item 3.**
- **Merge storms.** Four concurrent children on one document all merge at the
  end. The post-order scheduler serializes them, so the failure mode is latency,
  not corruption — but a merge that conflicts returns typed conflicts to the
  *parent model*, and four conflict payloads in one turn will blow the context.
  Clamp the conflict payload the same way tool text is clamped.
- **Live snapshots are lossy across a restart.** A child running when the app
  dies leaves an `active` workspace row and a thread with no live snapshot. React
  papers over this by rewriting `running` to `aborted` at load
  (`subagent-state.ts:170-190`). In Design 2 the honest fix is a sweep at
  startup: any `authored_subagent_workspaces` row whose owning thread has no
  in-flight turn is retired, its thread marked ended. `thread_cleanup.rs` already
  does the analogous sweep for Python processes.
- **Depth-2 by registry arm** is only as strong as the registry. Whoever adds the
  third `AgentKind` must not hand the child registry the subagent tool. Worth a
  test asserting `registry(kind).names()` excludes `"subagent"` for the child
  case — the same shape as the existing surface check the module docs mention.
- **The pill is live-only state.** Reloading the thread mid-run shows chips with
  no pill until the next snapshot arrives. Acceptable; the alternative is
  persisting running state, which is exactly the milestone-row mistake.
