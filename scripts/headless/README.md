# Headless harness

Runs Luma's backend command surface — and, in the E2E wave, the frontend agent
code that calls it — without a window, an `AppHandle`, or an audio device.

Two halves:

| | |
|---|---|
| `src-tauri/src/bin/agent_harness.rs` | stdio JSON-RPC server over the command surface |
| `scripts/headless/shim.ts` | spawns it and installs `window.__TAURI_INTERNALS__` so unmodified frontend `invoke()` calls land on the pipe |
| `scripts/headless/smoke.ts` | end-to-end check of both halves against a copy of the real `luma.db` |
| `scripts/headless/e2e.ts` | drives the *real frontend agents* and audits the design's §22 acceptance criteria |

## Build + run

```sh
cargo build --manifest-path src-tauri/Cargo.toml --bin agent_harness
bun run scripts/headless/smoke.ts
bun run scripts/headless/e2e.ts
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

**Scores** — `list_scores_for_track`, `create_score`, `list_track_scores`,
`create_track_score`, `update_track_score`, `delete_track_score`,
`replace_track_scores`

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
