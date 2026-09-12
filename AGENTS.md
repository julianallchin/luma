# Repository Guidelines

Luma is a native GPUI desktop app. The UI lives in `gpui/crates/app` and `gpui/crates/ui`; the wgpu renderer is in `gpui/crates/render`. GPUI consumes the shared Rust backend in `backend/` through its dispatch seam. The backend has no Tauri dependency. `www/` is a separate documentation website.

## Shared checkout

Several agents work in this tree at once. Never run `git stash`, `git checkout --`, `git reset`, `git clean`, or tree-wide `cargo fmt` — each reverts or rewrites other agents' uncommitted work. Stage by path, format only files you touched, commit only your own files.

## Project Structure and Commands

- `gpui/`: native UI, renderer, scene model, and test harness. Read `gpui/BUILD.md` for prerequisites and build-cache conventions.
- `backend/`: shared backend, models, append-only SQLite migrations, and Python analysis workers.
- `resources/`: shared fixture definitions and meshes.
- `harness/`: native renderer goldens, reference captures, fonts, and image comparison tools.
- `www/`: independent documentation site; use Bun for its JavaScript tooling.

Run the app with `cargo +1.97.1 run --manifest-path gpui/Cargo.toml -p luma-app`.
Check it with `cargo +1.97.1 check --manifest-path gpui/Cargo.toml --workspace --all-targets`.
Run targeted tests with `cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p <crate>`.
Format only touched Rust files; never run tree-wide formatting in this shared checkout.

## Code Philosophy

Delete dead code — don't comment it out, don't keep it "just in case". If something is being replaced, remove the old thing entirely. No backwards-compatibility shims unless there's a concrete reason (e.g. a migration that must stay). When changing something fundamental, change it all the way.

Every change earns its complexity. Aim for elegant, simple diffs that compose well with what's already there. Reach for an abstraction only when it compresses real duplication or unifies a concept — not preemptively. Encapsulate invariants at the layer that owns them: enforce them inside the type, function, or DB constraint that's actually responsible, not scattered across callers (a TOCTOU pre-check from a caller is almost always weaker than a constraint enforced atomically below). If the same idea can be expressed with one less concept, do.

When you spot a smell adjacent to your work — a leaky abstraction, a guard that only fires on the happy path, error handling that hides the original cause, a comment papering over rot, dead branches — flag it explicitly in your response. You don't have to fix everything in one pass, but the human reviewing your work should know what you saw and chose not to touch.

## Coding Style

Use standard Rust formatting and clippy. Keep backend modules cohesive around domains. Native frontend calls go through `luma_lib::dispatch`; do not add a webview UI.

The backend retains `ts-rs` type metadata for shared schemas, but there is no desktop TypeScript consumer. Do not recreate `src/` for frontend work.

## Migrations

- Migrations are **append-only** once run on any machine: sqlx checksums each version (SHA-384), so editing an applied file breaks every launch with a checksum mismatch.
- To amend a migration, add a new timestamped file — never edit or renumber an existing one.
- Keep `backend/migrations` (local SQLite) and `supabase/migrations` (remote Postgres) symmetric: a schema change that syncs needs a file in both.

## Testing Guidelines

Use the GPUI harness for UI verification and `luma-render` tests/captures for rendering. Run backend tests for backend changes. Keep migrations consistent with model changes.

## Data & File Locations

The global library database `luma.db` stays in the platform app config directory:

- macOS: `~/Library/Application Support/com.luma.luma/luma.db`
- Windows: `%APPDATA%\\com.luma.luma\\luma.db`
- Linux: `~/.config/com.luma.luma/luma.db`

Venues, scores, patterns and the `changes` log all live in that one database — there is no per-venue project file. Sync replicates those rows through PowerSync; see [docs/design/sync.md](docs/design/sync.md).

## UI Conventions

Comet (https://github.com/zeronsh/comet) is the button-style reference. Use
`luma_ui::button` for compact actions, `luma_ui::float::btn` / `btn_primary`
for dialog actions, and the shared chip/segment primitives for triggers and
toggles. This applies to panels and toolbars as well as floating surfaces.
Buttons have rounded corners, normal-case labels and subtle hover fills.
Never recreate the removed square, bordered, 9px uppercase button style or
copy controls from the obsolete web frontend. Keep styling in the shared UI
primitives so fixes reach every caller.

Use native GPUI confirmation dialogs for destructive actions.

Always use Nucleo icons for new or replaced icons. The local bundle lives at `~/github/nucleo` (MCP: `mcp/dist/index.js`; skills: `skills/nucleo-icons/SKILL.md`). Copy chosen SVGs into the app’s embedded assets; never require an absolute home-directory path at runtime.

## Releases

The obsolete Tauri webview release workflow has been removed. Native GPUI packaging must be configured before publishing a new release.

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
- **Selection expressions** in scores reference group names with boolean operators (`front_wash & left_movers`, `drum_uplighters | dj_booth > back_wash`). The `all` keyword selects every fixture. At runtime, the expression resolves to whichever fixtures belong to the named groups in the current venue.
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
- [Node Reference](https://luma.show/docs/node-reference) — Complete reference for all pattern graph node types
- [Architecture](https://luma.show/docs/architecture/overview) — Signal system, node graph engine, compositor, DMX pipeline, selection system
- [Glossary](https://luma.show/docs/glossary) — Canonical terms used throughout the codebase
