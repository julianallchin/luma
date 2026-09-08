# Venue and show MCP gauntlet

The Gauntlet Loop is documented in `docs/specs/venue-builder-gauntlet.md` §1:
an owner produces fresh evidence, a separate critic returns SHIP IT or FAIL,
and failures return to the owner for a new round. The current user requirement
supersedes the older spec's coordinate-free facade and tile-map-first design:
headless 3D previews, isolated groups and full-draft compositing are acceptance
requirements here.

This driver uses the installed Claude CLI as an external agent with only
Luma's **actual stdio MCP tools**. A transparent recording proxy captures the
complete request/response stream. There is no substitute agent loop, mocked
image, copied production database or cloud synchronization. CLI subscription
authentication stays with the CLI; no credentials are copied to the library
or evidence. The model defaults to the CLI's configured default; `--model`
is an explicit override for a requested model.

```sh
cargo +1.97.1 build --manifest-path backend/Cargo.toml --bin agent_harness --bin luma-mcp
python3 scripts/headless/gauntlet-mcp-proxy.test.py
bun scripts/headless/venue-show-gauntlet.ts prepare
# Use the temporary run directory printed above:
bun scripts/headless/venue-show-gauntlet.ts venue --run /tmp/luma-gauntlet-EXAMPLE
bun scripts/headless/venue-show-gauntlet.ts show --run /tmp/luma-gauntlet-EXAMPLE
bun scripts/headless/venue-show-gauntlet.ts inspect --run /tmp/luma-gauntlet-EXAMPLE
```

`prepare` uses the shared dispatcher to create an empty venue, then seeds a
synthetic 64-second PCM pulse track and known beat grid in the disposable
database. It does not build a rig or author a score. An isolated cache directory
links to the existing managed Python environment (`--cache-dir` chooses its
source), with bytecode writes disabled and a private Matplotlib cache. Worker
sources come from the repository; real fixture resources are resolved
by the headless host. Build commands should be serialized with other agents
using the same Cargo target directory.

Each phase starts a fresh external agent. The show agent receives no builder
conversation, so it must independently understand the persisted room. A failed
phase retains all evidence; the same phase cannot silently overwrite a prior
round. Start a fresh `prepare` after fixes. `--prompt-file` supports alternate
venue/show scenarios without editing the driver.

Before spending a model turn, each phase handshakes with the actual MCP server,
opens its scope, and runs a Python `describe()` through the same recording
proxy. Any server crash, timeout, binding error or missing Python dependency
stops the phase. This preflight creates no venue geometry or lighting; opening
the show scope may create its empty score through the normal MCP operation.
Its separate `preflight/` trace and exit report remain with the phase evidence.

Evidence stays in the run directory:

- `gauntlet.json`: isolated library and subject IDs.
- `venue/` and `show/`: exact prompt, CLI JSONL, MCP JSONL, server stderr,
  and content-addressed image files. The MCP trace preserves base64 exactly.
  `build.json`, `source-state.diff` and `source-files.json` identify the binary,
  commit and source state used when each phase starts, including backend crates.
  `harness-source/` retains the driver, proxy and prompt sources;
  `resource-state.json` records the tracked resource tree, resource worktree
  status and submodule commits without hashing the large fixture assets.
- `snapshot.json`: venue/fixture/group, score and persisted conversation rows.
- `tool-metrics.json`: tool names, response size, elapsed time, error flags,
  image paths linked to the exact MCP request ID, and candidate diagnostic
  lines (including caught Python errors in otherwise successful responses).
- `trajectory.jsonl`: exact input and response text with image paths instead
  of base64 for concise critic review. Raw `mcp.jsonl` remains authoritative.
- `library/`: retained SQLite state and audio; never a real app config path.

The run root's `tool-metrics.json` and `trajectory.jsonl` are cumulative across
venue and show, identified by each event's `stage`. The copies inside `venue/`
and `show/` contain only that phase's events. Each phase's `snapshot.json`
captures the complete library state when that phase ended.

Assign a fresh independent critic `critic.md`, the two prompts, and this
evidence directory. The driver records evidence; it **does not declare visual
acceptance**. For a complete pass the critic must inspect actual image files,
group memberships, score targeting, and the full trajectory. Exporting the
snapshot or finishing the CLI successfully is not itself a pass. Keep the
result and ranked defects with the round before making fixes.

Freeze backend/Python/renderer source files while a phase runs. The MCP server
checks source freshness on every authoring call, so editing source after the
binary build makes the running server refuse further authoring. Rebuild both
binaries after fixes before starting the next round.

This fixture tests placement, targeting, previews and compositing. It does
not test music analysis quality. Use a separately authorized audio fixture
for that surface. Native parent/child scheduling is also a separate runtime
test: the public stdio MCP server has no native subagent tool.
