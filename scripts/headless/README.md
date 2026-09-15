# Headless tools

These tools run Luma's backend without a window. The binaries live in
`backend/src/bin/`. The scripts in this directory drive them.

## Build

```sh
cargo +1.97.1 build --manifest-path backend/Cargo.toml \
    --bin agent_harness --bin luma-mcp --bin luma-agent --bin render_venue
```

Scripts look for the binaries in `backend/target/debug/`. Set `LUMA_MCP_BIN` or
`LUMA_AGENT_BIN` to use another build.

## Host options

`agent_harness`, `luma-mcp` and `luma-agent` boot through
`luma_lib::headless_host` and share these options:

| flag | env | default |
|---|---|---|
| `--config-dir` | `LUMA_CONFIG_DIR` | the app config directory (the real library) |
| `--fixtures-root` | `LUMA_FIXTURES_ROOT` | `resources/fixtures` in the repository |
| `--cache-dir` | — | the app cache directory, which holds the managed Python venv |
| `--fixture-principal` | — | the signed-in account |

Migrations run on the given config directory at startup. An empty directory
gives a new library.

`--fixture-principal` sets a trusted owner without a Supabase session. It
requires `--config-dir`. Use it only with a disposable library.
`scratch-library.ts` re-homes the rows of a copied library to such an owner.

The default config directory is the real library. Commands write to it.

## `agent_harness`: the dispatch surface

One JSON request per line on stdin. One JSON response per line on stdout.

```
->  {"id": 1, "cmd": "list_patterns", "args": {}}
<-  {"id": 1, "ok": [ ... ]}
<-  {"id": 1, "err": "message"}
```

- Every command goes through `luma_lib::dispatch`, with the same handler and
  arguments as the app. `docs/specs/ipc-manifest.md` lists the commands.
- Each request runs in its own task. Responses can arrive out of order; match
  them by `id`.
- A malformed line gets `{"id": null, "err": ...}`. The process stays up.
- Events go to stderr.
- There is no Art-Net output, no audio device, no render loop and no sync loop.

To add a command, add it to `dispatch`. The harness needs no change.

## `luma-agent`: one agent turn

```sh
backend/target/debug/luma-agent --thread THREAD_ID --prompt 'Continue the score'
backend/target/debug/luma-agent --track TRACK_ID --venue VENUE_ID \
    --engine claude --prompt 'Author the whole track' --sync
```

| flag | meaning |
|---|---|
| `--thread ID` | Continue a conversation. |
| `--track ID --venue ID` | Create a score and a conversation. |
| `--scope JSON` | Create a conversation from a serialized `ThreadScope`. |
| `--prompt TEXT` | Required. |
| `--engine api\|codex\|claude` | `api` uses Luma's provider settings. `codex` and `claude` use the installed CLIs and their own accounts. |
| `--model NAME`, `--effort LEVEL\|auto` | Override the thread's selection. |
| `--sync` | Sync before and after the turn. |

The turn uses the same service as the in-app chat. Turn events go to stdout as
JSON lines. The score and thread ids go to stderr.

### `author_score.ts`

```sh
bun run scripts/headless/author_score.ts <track> <venue> [--runner claude|codex] [--model MODEL]
bun run scripts/headless/author_score.ts --usage-only [--runner claude|codex]
```

The script resolves a track and a venue by id or name with `luma-mcp`'s `find`
tool. It checks the subscription quota and then starts `luma-agent` with a
fixed authoring prompt. It passes `--sync` unless `--fixture-principal` is set.

| flag | meaning |
|---|---|
| `--max-weekly F` | Do not start at or above this weekly usage fraction. Default `0.5`. |
| `--skip-usage-check` | Skip the quota check before and after the run. |
| `--usage-only` | Print the quota and exit. |
| `--config-dir`, `--cache-dir`, `--fixtures-root`, `--fixture-principal` | Passed to the host. |

Exit code `75` means the quota is spent. A failed turn exits `1`.

The quota comes from these modules:

- `usage.ts`: the `PlanUsage` shape and its one-line summary.
- `claude-usage.ts`: Claude subscription windows, from `/api/oauth/usage`.
- `codex-usage.ts`: ChatGPT plan windows, from `/wham/usage`.

## `luma-mcp`: MCP over stdio

`luma-mcp` gives an external coding agent Luma's Python workspace and skills.

| tool | effect |
|---|---|
| `find {track?, venue?}` | List matching tracks and venues. Writes nothing. |
| `open {track_query\|track_id, venue_id?}` | Bind a track in a venue. With only `venue_id`, bind the venue. |
| `python {code}` | Run a cell. Returns output, tracebacks and figures. |
| `reset {}` | Start a new workspace and kernel. |
| `cancel {}` | Interrupt the running cell. |
| `skill {name}` | Return one playbook. |

The server also answers `prompts/list` and `prompts/get` with the skills.
`luma-mcp record-usage --json '<AgentThreadUsage>'` stores usage for a thread.

Before `open` and `python`, the server checks whether its executable was
rebuilt. If it was, the client must restart the MCP connection.

- `mcp-client.ts`: the client side of MCP stdio, used by the other scripts.
- `mcp_smoke.ts`: runs the protocol against a copy of the real library.

## `render_venue`: render a saved venue

```sh
cargo +1.97.1 run --manifest-path backend/Cargo.toml --bin render_venue -- \
    --venue-id UUID --output /tmp/venue.png --view front
```

| flag | meaning |
|---|---|
| `--db PATH` | Library to read. Default: the app library. |
| `--state PATH` | State database. |
| `--format png\|catalogue` | An image, or a renderer scene catalogue as JSON. |
| `--view` | `front`, `audience`, `overhead`, `quarter_left`, `quarter_right` or `dj`. |
| `--width`, `--height` | 1–2000 pixels. Default 1600 × 1000. |

It opens SQLite read-only and starts no session.

## Venue and show gauntlet

See [harness/agent-gauntlet/README.md](../../harness/agent-gauntlet/README.md).

- `venue-show-gauntlet.ts`: phases `prepare`, `venue`, `show` and `inspect`.
- `gauntlet-mcp-proxy.py`: passes MCP frames through unchanged and records
  them to `mcp.jsonl`. It saves image blocks to `images/`.
- `gauntlet-mcp-smoke.py`: checks MCP boot, binding and a Python cell before a
  paid model turn.
- `gauntlet-mcp-proxy.test.py`: tests the recorder.

## Known broken

- `import_engine_playlists.ts` imports `./shim`, which does not exist.
- `mcp_smoke.ts` counts rows in `authored_revisions`, which the row-model
  migration drops. It also expects 10 skill prompts; `resources/skills` has 12.
