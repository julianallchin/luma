# Repository Guidelines

Luma is a native GPUI desktop app. The UI lives in `gpui/crates/app` and `gpui/crates/ui`; the wgpu renderer is in `gpui/crates/render`. GPUI consumes the shared Rust backend in `backend/` through its dispatch seam. `www/` is a separate documentation website.

## Shared checkout

Several agents work in this tree at once. Never run `git stash`, `git checkout --`, `git reset`, `git clean`, or tree-wide `cargo fmt` — each reverts or rewrites other agents' uncommitted work. Stage by path, format only files you touched, commit only your own files.

## Project Structure and Commands

- `gpui/`: native UI, renderer, scene model, and test harness. Read `gpui/BUILD.md` for prerequisites and build-cache conventions.
- `backend/`: shared backend, models, append-only SQLite migrations, and Python analysis workers.
- `resources/`: shared fixture definitions and meshes.
- `harness/`: test reference data: renderer goldens, reference captures, style specs, and image comparison tools.
- `assets/`: the app icon and the fonts the app embeds.
- `www/`: independent documentation site; use Bun for its JavaScript tooling.

A fresh clone needs `scripts/setup` once. `rust-toolchain.toml` pins the toolchain for both workspaces, so plain `cargo` is right from anywhere in the repository. Run commands from the repository root.

Run the app with `python3 gpui/dev.py` (rebuilds on save) or `cargo run --manifest-path gpui/Cargo.toml -p luma-app`.
Check it with `cargo check --manifest-path gpui/Cargo.toml --workspace --all-targets`.
Run targeted tests with `cargo test --manifest-path gpui/Cargo.toml -p <crate>`; backend tests with `cargo test --manifest-path backend/Cargo.toml --workspace`; the agent executor's Python tests with `python3 backend/python/luma_exec/tests/run_tests.py`. The README's "Tests" table lists the rest.
Format only touched Rust files (`rustfmt <file>`; `rustfmt.toml` sets the edition); never run tree-wide formatting in this shared checkout.

## Where things go

Every committed file has a home and a reader. Before adding one, find its place here; if none fits, ask.

- **One-off scripts, probes, repros, measurements, scratch data** go in `experiments/` (ignored by git) or your scratch directory. Never commit them. A finding worth keeping goes in a doc or a test, not a script.
- **Rust programs people run** (`luma-cli`, the app) are `src/bin/` of their crate. A developer tool for one crate (a profiler, a golden renderer, a lab) is a `src/bin/` or `examples/` of the crate it exercises, documented in that crate's README or `gpui/BUILD.md`.
- **Scripts** go in `scripts/<area>/` (`setup`, `release/`, `headless/`, `library/`), each listed in a README with what it is for. A script that only one task needed is a one-off: see the first rule.
- **Tests** live with the code they test: Rust unit tests in the module, integration tests in the crate's `tests/`, UI tests as `.test.js` under `gpui/crates/agent/tests/js/`, Python tests under `backend/python/luma_exec/tests/`.
- **Test reference data** (goldens, reference captures, style specs) goes in `harness/`. Assets the app ships go in `assets/` or `resources/`.
- **Docs**: how a system works and why goes in `docs/design/` or `docs/specs/` (indexed in `docs/README.md`); user-facing docs go in `www/`; build and test know-how in `gpui/BUILD.md` or the crate's README. No dated handoff notes, audits or friction logs: fix the thing, or put the knowledge where the next reader looks.
- **Names**: Rust programs are kebab-case (`luma-cli`), Rust files and Python files snake_case, docs kebab-case.

Delete what you added once it has served its purpose.

## Code Philosophy

Delete dead code — don't comment it out, don't keep it "just in case". If something is being replaced, remove the old thing entirely. No backwards-compatibility shims unless there's a concrete reason (e.g. a migration that must stay). When changing something fundamental, change it all the way.

Every change earns its complexity. Aim for elegant, simple diffs that compose well with what's already there. Reach for an abstraction only when it compresses real duplication or unifies a concept — not preemptively. Encapsulate invariants at the layer that owns them: enforce them inside the type, function, or DB constraint that's actually responsible, not scattered across callers (a TOCTOU pre-check from a caller is almost always weaker than a constraint enforced atomically below). If the same idea can be expressed with one less concept, do.

When you spot a smell adjacent to your work — a leaky abstraction, a guard that only fires on the happy path, error handling that hides the original cause, a comment papering over rot, dead branches — flag it explicitly in your response. You don't have to fix everything in one pass, but the human reviewing your work should know what you saw and chose not to touch.

## Coding Style

Use standard Rust formatting and clippy. Keep backend modules cohesive around domains. Native frontend calls go through `luma_lib::dispatch`; do not add a webview UI.

## Migrations

- Migrations are **append-only** once run on any machine: sqlx checksums each version (SHA-384), so editing an applied file breaks every launch with a checksum mismatch.
- To amend a migration, add a new timestamped file — never edit or renumber an existing one.
- Keep `backend/migrations` (local SQLite) and `supabase/migrations` (remote Postgres) symmetric: a schema change that syncs needs a file in both.
- Give each migration a version prefix no other file in its folder uses, and give a synced change the same file name in both folders.
- A Supabase migration also gets its line in `deploy/README.md`, with what must be deployed before or after it.

## Testing Guidelines

Use the GPUI harness for UI verification and `luma-render` tests/captures for rendering. Run backend tests for backend changes. Keep migrations consistent with model changes.

- **Write UI tests as `.test.js`** under `gpui/crates/agent/tests/js/` and run them with `gpui/test` (see `gpui/BUILD.md`, "Script tests"). They need no Rust build. Keep a UI test in Rust only when it needs Rust-side setup or measurement.
- **Assert behavior, not values copied from the code.** A test that fails when someone tunes a constant, moves a pixel or renames an internal, while nothing a user sees broke, is a change-detector: delete it or assert the relation behind it (an order, a bound, a round trip, "lit > dark").
- **No absolute timings and no sleeps.** Wait with `until`; compare timings against a measurement from the same run.
- **Never loosen a test or raise a timeout to make it pass.** A skipped test says why: `test.skip("bug: …")`.
- **Run the tests you changed**, not the whole suite.
- **Goldens are check-only.** A stale golden fails and is left alone; regenerate it with `LUMA_UPDATE_GOLDENS=1`, then review and commit the diff. A new golden test follows the same rule.

## Data & File Locations

The global library database `luma.db` stays in the platform app config directory:

- macOS: `~/Library/Application Support/com.luma.luma/luma.db`
- Windows: `%APPDATA%\\com.luma.luma\\luma.db`
- Linux: `~/.config/com.luma.luma/luma.db`

Venues, scores and patterns all live in that one database — there is no per-venue project file. Sync replicates those rows through PowerSync; see [docs/design/sync.md](docs/design/sync.md).

## UI Conventions

The current style is the rounded, simple, glass look. Comet
(https://github.com/zeronsh/comet) is the reference. `luma_ui::glass` holds
the shared glass colors and surfaces. Use `luma_ui::button` for compact
actions, `luma_ui::float::btn` / `btn_primary` for dialog actions, and the
shared chip/segment primitives for triggers and toggles. This applies to
panels and toolbars as well as floating surfaces. Controls have rounded
corners, normal-case labels and subtle hover fills. Do not make square,
bordered, 9px uppercase or silkscreen-style controls. Keep styling in the
shared UI primitives so fixes reach every caller.

Use native GPUI confirmation dialogs for destructive actions.

Always use Nucleo icons for new or replaced icons. The local bundle lives at `~/github/nucleo` (MCP: `mcp/dist/index.js`; skills: `skills/nucleo-icons/SKILL.md`). Copy chosen SVGs into the app’s embedded assets; never require an absolute home-directory path at runtime.

## Releases

A `v*` tag matching `backend/Cargo.toml`'s version builds a draft release; a manual run of the Release workflow builds the `nightly` prerelease. Installed apps update themselves from those releases (`gpui/crates/update`). An installed build reads its runtimes, workers and fixtures from a bundled resource directory (`backend/src/resources.rs`); a new runtime file needs a line in `scripts/release/stage-resources`. See [RELEASING.md](RELEASING.md) for the steps and the secrets.

## Commit & Pull Request Guidelines

Follow conventional, imperative commits (e.g., `add track annotation drag`, `fix waveform cache`). PRs should include:

- a clear summary of behavior changes,
- linked issues or context,
- screenshots/video for UI changes,
- notes on any schema/migration impacts.

## Groups & Selection

Groups are the core mechanism for targeting fixtures in scores. A score never references specific fixtures — it references **group names**, and each venue defines its own groups of physical fixtures.

### How it works

- **Groups** are user-created collections of fixtures within a venue (e.g., `front_wash`, `drum_uplighters`, `back_movers`). Each group has a snake_case name and optional axis positions (LR/FB/AB) for spatial selection.
- **Selection expressions** in scores reference group names with boolean operators (`front_wash & left_movers`, `drum_uplighters | dj_booth > back_wash`, `all & ~back_wash`). The `all` keyword selects every fixture. At runtime, the expression resolves to whichever fixtures belong to the named groups in the current venue.
- **Venue portability**: When moving a score between venues, an LLM can remap group names (e.g., `front_wash` in venue A → `house_pars` in venue B).

### Group naming

Group names are automatically normalized to snake_case: lowercase, spaces/hyphens become underscores, non-alphanumeric characters are stripped. Names must match `[a-z][a-z0-9_]*` and cannot be `all`.

### Key files

- `backend/src/models/groups.rs` — `FixtureGroup`, name normalization/validation helpers
- `backend/src/services/groups.rs` — hierarchy building, selection expression parser/evaluator, spatial filtering
- `backend/src/database/local/groups.rs` — group CRUD, membership
- `backend/src/dispatch/handlers/groups.rs` — group command handlers
- `gpui/crates/app/` — native group management UI
- `gpui/crates/ui/src/arg/expression.rs` — group selection expression editor

---

## Documentation

- [User Guide](https://luma.show/docs/user-guide/why-luma) — Why Luma exists, venues, groups & tags, patterns, annotations, performing
- [Node Reference](https://luma.show/docs/node-reference) — Pattern graph nodes, one generated page per category: audio, aim, color, forms, masks, math, output, space, time and events
- [Architecture](https://luma.show/docs/architecture/overview) — Node graph engine, compositor, DMX pipeline, fixture system, selection system, database, import pipeline, design decisions
- [Glossary](https://luma.show/docs/glossary) — Canonical terms used throughout the codebase
