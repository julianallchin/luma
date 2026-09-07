# Headless harness

Runs Luma's backend command surface — and, in the E2E wave, the frontend agent
code that calls it — without a window, an `AppHandle`, or an audio device.

The pieces:

| | |
|---|---|
| `src-tauri/src/bin/agent_harness.rs` | stdio JSON-RPC server over the command surface |
| `scripts/headless/shim.ts` | spawns it and installs `window.__TAURI_INTERNALS__` so unmodified frontend `invoke()` calls land on the pipe |
| `scripts/headless/smoke.ts` | end-to-end check of both halves against a copy of the real `luma.db` |
| `scripts/headless/e2e.ts` | drives the *real frontend agents* and audits the design's §22 acceptance criteria |
| `src-tauri/src/bin/luma-mcp.rs` | MCP over stdio: the same sandboxed Python workspace, for an out-of-process coding agent |
| `scripts/headless/mcp_smoke.ts` | speaks MCP at that binary over a pipe — handshake, `open`, `python`, figures, `reset` |
| `scripts/headless/author_score.ts` | resolves a show job and launches the shared Rust agent runtime |
| `scripts/headless/import_engine_playlists.ts` | imports whole Engine DJ playlists into a real library and stays up until the analysis DAG finishes |
| `scripts/headless/usage.ts` | the plan-usage shape the gate reads, and its summary line |
| `scripts/headless/claude-usage.ts` | the Claude subscription's rate-limit windows, as that shape |
| `scripts/headless/codex-usage.ts` | the ChatGPT plan's rate-limit windows, as that shape |

## Build + run

```sh
cargo build --manifest-path src-tauri/Cargo.toml --bin agent_harness
bun run scripts/headless/smoke.ts
bun run scripts/headless/e2e.ts

cargo build --manifest-path src-tauri/Cargo.toml --bin luma-mcp
bun run scripts/headless/mcp_smoke.ts
```

The smoke driver creates its own scratch config dir under `$TMPDIR`, copies the
real `~/Library/Application Support/com.luma.luma/luma.db` into it (read-only
source — every migration and mutation lands on the copy), and deletes the
scratch dir when it finishes. If there is no real library it still runs, with
the data-dependent sections reported as `SKIP`. Exit code is non-zero on any
`FAIL`.

## Protocol

One JSON object per line in, one per line out:

```
->  {"id": 1, "cmd": "list_patterns", "args": {}}
<-  {"id": 1, "ok": [ ... ]}
<-  {"id": 1, "err": "human-readable message"}
```

Requests are dispatched **concurrently**, one task each, exactly as Tauri's IPC
does — `cancel_python_cell` only means anything while a `run_python_cell` is
still in flight. Responses are matched by `id`, so they may come back out of
order.

`args` keys are the same camelCase the frontend passes to `invoke` (snake_case
is accepted too). A malformed frame answers `{"id": null, "err": ...}` and the
process stays up — bad input never kills a long-lived harness.

### Setup

| | flag | env | default |
|---|---|---|---|
| config dir | `--config-dir` | `LUMA_CONFIG_DIR` | `StorageRoot::from_env_default()` (the real app config dir) |
| fixtures root | `--fixtures-root` | `LUMA_FIXTURES_ROOT` | newest `resources/fixtures/*`, resolved relative to the repo |
| cache dir | `--cache-dir` | `LUMA_CACHE_DIR` | `dirs::cache_dir()/com.luma.luma` (where the managed venv lives) |
| fixture owner | `--fixture-principal` | — | verified principal from `state.db` |

Migrations run on startup against whatever config dir it is given, exactly as
the app does — pointing it at an empty directory produces a fresh, fully
migrated `luma.db` + `state.db`.

`--fixture-principal` is a harness-only trusted identity seam and requires an
explicit `--config-dir`. Use it only with a disposable database whose rows were
re-homed to that same principal; it arms the normal app-database write gate
without copying a live Supabase token.

## Pointing it at your own scratch dir

```sh
mkdir -p /tmp/luma-scratch
cp "$HOME/Library/Application Support/com.luma.luma/luma.db" /tmp/luma-scratch/
./src-tauri/target/debug/agent_harness --config-dir /tmp/luma-scratch
```

…then type request lines on stdin. From Bun:

```ts
import { startHarness } from "./scripts/headless/shim";

const h = await startHarness({ configDir: "/tmp/luma-scratch" });
console.log(await h.invoke("list_patterns"));
await h.close();
```

## The acceptance driver (`e2e.ts`)

`bun run scripts/headless/e2e.ts` drives the real frontend agent modules —
`resolveThread`, `buildGraphAgentTools`, `buildPythonTool`,
and `trackAgent` itself — against this harness, and prints a PASS/FAIL/SKIP
table for the 33 acceptance criteria in
`docs/design/agent-code-execution.md` §22.

- **Phase 1** (always) needs no model: it calls the tools' `execute()` directly
  with real code and asserts observable behavior against real library data.
- **Phase 2** (optional) runs one real track-copilot turn. It looks for
  `OPENROUTER_API_KEY`, else the app's own key in WebKit's localStorage. With no
  key — or when the provider refuses (no credits, bad key) — it reports SKIP.
  Set `LUMA_E2E_PHASE1_ONLY=1` to prohibit provider access even when a stored
  key exists.

Isolation matches `smoke.ts`: scratch config dir seeded from a copy of the real
`luma.db`, with `tracks/` symlinked (tens of GB of audio, read-only on every
agent path). Agent workspaces land under the scratch config dir, which the run
asserts. `e2e.ts` re-homes authored rows to a synthetic fixture owner so it can
exercise the real mutation gate without copying `state.db` or any auth secret.
The cache dir points at the real `~/Library/Caches/com.luma.luma` so the managed
venv is reused rather than rebuilt.

## Driving the real agents (E2E wave)

`startHarness()` installs the globals as a side effect, so **frontend modules
must be imported after it resolves**. A top-level `import` is hoisted above the
`await` and would capture an un-shimmed global:

```ts
const h = await startHarness({ configDir: scratch });

// dynamic import — after the globals exist
const { buildTrackAgentTools } = await import("@/features/track-editor/agent/tools");

const tools = buildTrackAgentTools(/* … */);
await tools.get_track_beats.execute({ trackId });   // → real Rust, real DB
```

Alongside `__TAURI_INTERNALS__` the shim provides the browser globals the agent
modules touch: `localStorage` and `window.addEventListener` / `dispatchEvent`
(used by `openrouter-key.ts`), plus `convertFileSrc`, which returns a `file://`
URL instead of the app's `asset:` URL.

Known gaps for that wave:

- `previewToPngBase64` (`src/features/track-editor/agent/preview-image.ts`) uses
  `OffscreenCanvas` + `ImageData`, which Bun does not implement. The heatmap
  commands themselves work headless (`preview_pattern_image`,
  `preview_graph_image`, `view_composite_image` all return raw pixels); only the
  browser-side PNG encoding step is missing. `e2e.ts` stubs it to a valid 1×1
  PNG so a tool path that reaches it completes, but the *image content* of any
  preview tool can only be checked in the real app. Fix is to move PNG encoding
  into Rust rather than to shim a canvas.
- `host_audio::*` needs a real device; not exposed.

## Supported commands

Unless marked host-only below, names and argument shapes match the Tauri
registration exactly.

**Agent threads** — `agent_thread_create`, `agent_thread_get`,
`agent_thread_list`, `agent_thread_append_messages`, `agent_thread_delete`,
`agent_thread_rename`

**Authored state** — `authored_state_prepare_turn`,
`authored_state_finalize_turn`, `authored_state_recover_turns`,
`authored_state_list_history`, `authored_state_restore`

**Host-only authored workspace harness** —
`authored_state_create_workspace`, `authored_state_check_workspace`,
`authored_state_commit_workspace`, `authored_state_merge_workspace`,
`authored_state_remove_workspace`

These five operations exercise the internal orchestration foundation against a
controlled scratch config. They are intentionally absent from Tauri IPC and
TypeScript bindings. Do not pass the returned absolute snapshot path to an
untrusted process: app subagents stay disabled until one supervisor owns a
sandboxed child process tree and holds its lease through exit, snapshot,
revision commit, and prune.

Workspace creation requires a caller-owned `requestId` and an exact
`expectedBaseRevisionId` selected from document history. Retrying the same
request is idempotent only when that base is unchanged.

**Agent code execution** — `run_python_cell`, `cancel_python_cell`. The kernel
runs against the managed venv under the cache dir; the harness never creates
one, so on a machine that has never run the app these are the only commands
that fail (with that reason).

**Patterns** — `list_patterns`, `get_pattern`, `get_pattern_graph_document`,
`get_pattern_args`, `save_pattern_graph_document`, `list_pattern_categories`

**Scores** — `list_scores_for_track`, `list_scores_across_venues`, `create_score`,
`list_track_scores`, `create_track_score`, `update_track_score`,
`delete_track_score`, `replace_track_scores`

**Tracks** — `list_tracks`, `list_tracks_enriched`, `get_track_beats`,
`get_track_waveform`, `get_track_bar_classifications`, `get_track_drum_onsets`,
`get_classifier_thresholds`

**Venue / fixtures** — `list_venues`, `get_venue`, `get_patched_fixtures`,
`get_grouped_hierarchy`, `list_groups`

**Graph** — `get_node_types`, `run_graph`, `preview_pattern_image`,
`preview_graph_image`, `view_composite_image`

Deliberate behavioral differences from the app, all of which are absent
side-effects rather than different results:

- `run_graph` does not install the result as the live scene (no `RenderEngine`).
  It does honor `agentThreadId`, publishing the evaluation for that thread's
  next Python cell.
- `get_patched_fixtures` does not push the patch to ArtNet (the app treats
  ArtNet as optional anyway).
- mutations do not poke the sync engine (no sync loop is running).

## Adding a command

Add a `match` arm in `agent_harness.rs` calling the same db/service function the
`#[tauri::command]` calls. If the command body is more than a delegation, move
the body into a `pub` service fn and have *both* call it — never transcribe
logic into the harness.

## Shared agent runtime

Build `luma-agent` to run the same Rust service as native chat:

```sh
cargo +1.97.1 build --manifest-path src-tauri/Cargo.toml --bin luma-agent
src-tauri/target/debug/luma-agent --thread THREAD_ID --engine codex --prompt 'Continue the score' --sync
src-tauri/target/debug/luma-agent --track TRACK_ID --venue VENUE_ID --engine claude --prompt 'Author the whole track' --sync
```

`--track` and `--venue` create a new score and conversation. `--thread` continues
one; `--scope` accepts a serialized `ThreadScope` for other agent kinds.
`--model` overrides the selected engine's model for this invocation. Host flags
are shared with the other headless binaries. Output is JSON `TurnEvent` lines;
score and thread ids go to stderr. Completed messages, tools, usage and authored
revisions use the same persistence path as chat. `--sync` pulls before execution
and syncs again afterwards; signed-in execution also needs the server claim RPC.

The API engine uses Luma's provider settings. The Codex and Claude engines use
unmodified, locally installed CLIs and their own signed-in subscription accounts.
They advertise Luma's scoped tools, including its subagent tool. Each child has
its own thread and authored workspace, exactly as in native chat. Provider
credentials and native session checkpoints stay on the machine.

A local file lock prevents concurrent processes from running a conversation.
The matching Supabase migration adds an atomic per-user, per-thread device claim:
another machine cannot take over a running conversation. Claims have no timed
expiry. After a crash, the originating machine can recover; another device must
wait for that machine to release the claim. A synced completed conversation can
continue on a new machine using its transcript as fresh context.

### Authoring by track and venue name

```sh
bun run scripts/headless/author_score.ts <track> <venue> \
    [--runner claude|codex] [--model MODEL]
```

This launcher resolves ids or substrings with `luma-mcp`'s read-only `find`,
checks the selected subscription's quota, then starts `luma-agent`. It contains
no agent loop or provider trace parser. Build both binaries first. Set
`LUMA_AGENT_BIN` or `LUMA_MCP_BIN` to use alternative builds.

It writes to the real library and syncs by default. Use `--config-dir` with
`--fixture-principal` for a disposable local fixture; fixture execution skips
cloud claims and sync.

| Flag | Behavior |
|---|---|
| `--max-weekly F` | Refuse to start at or above this weekly usage fraction (default `0.5`). |
| `--skip-usage-check` | Skip the launcher's preflight and post-run usage query. |
| `--usage-only` | Print the selected subscription's usage without starting a job. |

The launcher's preflight exits `75` when quota is exhausted. A failed running
turn exits `1`; there is no automatic retry or durable job queue. Interrupted
native sessions are invalidated so the next attempt reconstructs context from
completed Luma messages. In-flight assistant output is not yet crash-durable.

## Importing Engine DJ playlists (`import_engine_playlists.ts`)

```sh
bun run scripts/headless/import_engine_playlists.ts --dry-run
bun run scripts/headless/import_engine_playlists.ts
```

Imports whole Engine DJ playlists by title and then **stays up until the
analysis DAG finishes**. That waiting is the point: `engine_dj_import_tracks`
returns as soon as the rows are inserted and spawns beats / stems / MERT /
roots / drum-onsets / classifier / genre onto the *host's* runtime, so a
headless caller that exits on the command's reply kills its own analysis.

- **Library selection.** The real app config dir by default — the harness's own
  `StorageRoot::from_env_default()`. `--config-dir` (or `LUMA_CONFIG_DIR`)
  points it at a copy. `--library` is the Engine Library root.
- **Playlists.** `--playlist` is repeatable and defaults to a seven-playlist
  set; `Parent/Child` disambiguates a nested one. An ambiguous or unknown title
  fails the run before anything is imported.
- **Idempotent.** Already-imported tracks are filtered out on `source_id`
  (`<databaseUuid>:<engineTrackId>`) before the import call, so a re-run is a
  no-op. `--reprocess` additionally re-arms analysis for tracks whose DAG never
  finished — a headless host runs no startup reconcile, so nothing else picks
  those up.
- **Chunked, and each chunk analyzed before the next is imported** (`--chunk`,
  default 8). That is what makes progress reportable and a killed run
  resumable; concurrency *within* a batch is the backend's own affair, bounded
  on both branches by `analysis_worker_count()`.
- **Every harness call is bounded and retried.** The shim serializes its writes
  to the harness's stdin, so two concurrent `invoke`s can no longer splice one
  request line into another. If the harness still answers `{"id": null, ...}` —
  a frame it could not attribute to any request — the shim fails every
  in-flight call with that error rather than leaving them to hang.
- **Reporting.** Progress is `analyzed N/total` per artifact table from
  `list_tracks_enriched`; the final block adds waveform coverage and every
  `preprocessing_failures` row. Those two come from a read-only `bun:sqlite`
  handle on `luma.db` — neither has a command.

It runs against the machine's own signed-in account, read from the stored
session with `current_account`. Both that read and the principal a command
stamps its rows with tolerate an expired access token: a local write presents no
token, so expiry says nothing about it. Freshness is enforced where a token
actually goes to Supabase — `get_current_auth`, on the sync and upload paths —
and headless touches neither.

## The MCP server (`luma-mcp`)

Same bootstrap as the harness — same flags, same migrations, same managed venv,
same seatbelt sandbox — but the wire is MCP over stdio instead of the line
protocol above, so an out-of-process coding agent gets the `python` tool itself:
`open` a track, then `python`, `reset` and `cancel` against its persistent
kernel. See `docs/design/agent-code-execution.md` §20.1 for the tool contract
and a `.mcp.json` to register it with.

## Render a saved venue

From `src-tauri/`, run:

```sh
cargo run --bin render_venue -- --venue-id UUID --output /tmp/venue.png --view front
```

This reads the same solved venue and saved environment as the app. It opens
SQLite read-only, including committed WAL changes, and never starts an agent
thread, changes the library, or refreshes the app's credentials. Use `--db PATH`
for a scratch library, `--view overhead` for a plan, and `--width`/`--height` for
1–2000 pixel dimensions. The final JSON line names the image, venue, camera,
fixture count, and lighting environment. Fixtures are unlit; this command
checks the built room. Use `luma.venue.render(t=...)` in a score session to
inspect an evaluated lighting cue.

The MCP server checks its executable against source files before `open` and
`python`. If it reports stale code, rebuild **the binary**, then restart the
client's MCP connection. Rebuilding the library does not update a running
server. Cancel/reset remain available to finish an existing session safely.
