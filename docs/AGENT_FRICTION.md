# Agent friction log

Where agents grumble about tooling that fights back. One line each, newest first,
`- [YYYY-MM-DD] <gripe>`. Product bugs go in the task report, not here.

- [2026-08-27] the model-facing traceback in a headless transcript is truncated mid-frame, so the one line that names the failing host call is exactly the line you never get — reproducing the cell in a Rust test was faster than reading the log
- [2026-08-27] `luma.track` is reinstalled between cells but not by `apply()`, so a cell that applied and then rendered was told "track changed while this edit was open" by its own commit (fixed — apply advances the binding)
- [2026-08-27] the Python package has three test entry points (`test_track.py`, `test_venue.py`, `run_tests.py`) and no runner that knows about all three; nothing in AGENTS.md mentions the app venv at `~/Library/Caches/com.luma.luma/python-env/bin/python3` that they need
- [2026-08-27] biome ignores `scripts/`, so `bunx biome check --write scripts/headless/*.ts` prints "Checked 0 files" and exits 1 — the formatter you're told to run can't touch half the code you're asked to edit
- [2026-08-27] `claude --help` documents no `--max-turns`, yet the flag works — an agent capping a headless run has to guess that the SDK's option is still wired into the CLI
- [2026-08-27] AGENTS.md's build list never mentions `luma-mcp`, though `.mcp.json` points straight at `src-tauri/target/debug/luma-mcp` — you learn how to build it from an error string buried in `scripts/headless/mcp_smoke.ts`
- [2026-08-27] ugh, `.gitignore` ignores all of `/docs/*` with per-directory exceptions, so this very file was silently untracked and `git add` looked like it worked (fixed since, but the rule should be inverted)
- [2026-08-27] nothing tells another process that a document head moved, so the only way a second process learns it was behind is by losing a CAS it already did the work for
- [2026-08-27] `bin/luma-mcp` built services with a bare `Arc::new(boot(...))` and every `agent_turn_start` failed silently because the turn loop needs `into_shared()` (fixed since — `SharedServices` is a newtype now, so the bare form doesn't typecheck)
- [2026-08-27] lost an afternoon to the Python worker "hanging" — Bun hands children `RLIMIT_NOFILE = i64::MAX` and loky's fork path closes fds in a loop bounded by it (clamped in `worker.py` now, but anything else we spawn from Bun inherits it)
- [2026-08-27] `resources/` is found three different ways by three subsystems and every new binary reinvents the search, wrong on the first try — just give the crate one `resources_root()`
- [2026-08-27] burned time on a debounce that never fired: headless gpui virtualizes time, so `background_executor().timer()` hangs forever under `cargo test` while working fine in pixel mode
- [2026-08-27] spent a session on a "deaf" dialog before learning gpui computes focus from the rendered frame, so a stale handle still says `is_focused` and eats every key — test with `contains_focused`
- [2026-08-27] `git stash` is process-global, so concurrent agents in one checkout share a stack and another agent nearly popped my `stash@{0}` — use a worktree or a named branch
- [2026-08-27] one confusing sqlx compile error per new agent because rustup picks the toolchain from cwd, so `--manifest-path gpui/...` from the repo root silently runs 1.90 instead of the pinned 1.97.1
- [2026-08-27] every `chat` test collides with "database is locked" because the fixture db is keyed on the process id instead of the test name
- [2026-08-27] edited a migration file and broke every launch — sqlx checksums each version, and the only warning lived in a comment inside the migrations, not in AGENTS.md (fixed since)
- [2026-08-27] the ipc-manifest check writes the new files and *then* fails, so the first run after any command change is always red and the tree always dirty — and hand-written prose lives inside the generated file waiting to be clobbered
- [2026-08-27] ran a filtered cargo test and it nuked `schema.ts` down to the types those tests touched — had to run the whole suite just to get `bun run build` green again
