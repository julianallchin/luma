# Repository Guidelines

Luma is a Tauri desktop app: a React/TypeScript frontend in `src/` backed by a Rust core in `src-tauri/`. Most features span both halves via Tauri `invoke()` commands and shared TypeScript bindings.

## Project Structure & Module Organization

- `src/`: React 19 + TypeScript UI, Zustand stores, React Flow-based graph editors, Tailwind styling.
  - Feature modules live in `src/features/*` (e.g., `patterns/`, `track-editor/`, `app/`).
  - Shared UI/components in `src/shared/`.
- `src-tauri/`: Rust backend (entry `src-tauri/src/main.rs`, app setup in `src-tauri/src/lib.rs`).
  - Models in `src-tauri/src/models/`.
  - SQLite + migrations in `src-tauri/migrations/{app,project}`.
  - Python workers in `src-tauri/python/` for beats/roots/stems analysis.
- `projects/`: example venue projects (`*.luma`).
- `resources/fixtures/`: fixture definitions bundled into the app.
- `experiments/`: research notebooks and test data.

## Build, Test, and Development Commands

Use **Bun only** for JS tooling.

- `bun install`: install JS deps.
- `bun run dev`: run Vite dev server.
- `bun run tauri dev`: run desktop app with hot reload (Vite + Tauri).
- `bun run build`: typecheck + build frontend to `dist/`.
- `bun run tauri build`: produce distributable desktop build.
- `bun run lint`: Biome lint for TS + `cargo clippy` for Rust.
- `bun run format`: Biome format + `cargo fmt`.
- `cargo test --manifest-path src-tauri/Cargo.toml`: run Rust tests and regenerate TS bindings (see below).

## Coding Style & Naming Conventions

- TypeScript/React: formatted and linted by Biome (`biome.json`). Prefer functional components, hooks, and Zustand stores named `use-*-store.ts`. Files and folders are generally kebab-case; components are PascalCase.
- Rust: standard `rustfmt` + `clippy`. Keep backend modules cohesive around domains (`tracks`, `patterns`, `annotations`, `host_audio`).
- Cross-boundary API: add/rename Tauri commands in `src-tauri/src/lib.rs` and update frontend call sites under `src/features/**`.

## TypeScript Bindings (`ts-rs`)

Bindings are auto-generated from Rust structs in `src-tauri/src/models/` using `ts-rs`.

- Output file: `src/bindings/schema.ts` (ignored by git).
- Regeneration: happens automatically on `cargo test`.
- If you change exported Rust models, run `cargo test --manifest-path src-tauri/Cargo.toml` to refresh bindings.
- Do **not** commit generated `src/bindings/schema.ts`.

## Testing Guidelines

There is no dedicated JS test suite yet. Validate UI changes manually via `bun run tauri dev`. Rust changes should be covered with `cargo test` when possible. Keep migrations consistent with model changes.

## Data & File Locations

The global library database `luma.db` is stored in the Tauri app config directory:

- macOS: `~/Library/Application Support/com.luma.luma/luma.db`
- Windows: `%APPDATA%\\com.luma.luma\\luma.db`
- Linux: `~/.config/com.luma.luma/luma.db`

Venue projects are SQLite `.luma` files, created/opened via the UI and stored wherever the user chooses (samples in `projects/`).

## Commit & Pull Request Guidelines

Follow conventional, imperative commits (e.g., `add track annotation drag`, `fix waveform cache`). PRs should include:

- a clear summary of behavior changes,
- linked issues or context,
- screenshots/video for UI changes,
- notes on any schema/migration impacts.

## Groups & Tags

Groups and tags are the core mechanism that makes scores portable across venues. A score never references specific fixtures — it references **tags**, and each venue maps those tags to its own physical fixtures via groups.

### How it works

- **Groups** are user-created collections of fixtures within a venue (e.g., "Stage Left Movers", "Drum Riser Pars"). Each group has optional axis positions (LR/FB/AB) for spatial selection.
- **Tags** are labels drawn from a **predefined vocabulary** (spatial: `left`, `right`, `center`, `front`, `back`, `high`, `low`, `circular`; purpose: `blinder`, `wash`, `spot`, `chase`). Tags are assigned to groups, not individual fixtures.
- **Selection expressions** in scores reference tags with boolean operators (`left & wash`, `blinder | spot > par_wash`). At runtime, the expression resolves to whichever fixtures carry those tags in the current venue.

### Why predefined

The tag vocabulary is fixed (`PREDEFINED_TAGS` in `models/groups.rs`) so that scores and venues share a common language. If tags were freeform, a score written for one venue would silently match nothing on another. The predefined list ensures every venue speaks the same dialect.

### Key files

- `src-tauri/src/models/groups.rs` — `FixtureGroup`, `PREDEFINED_TAGS`, selection query types
- `src-tauri/src/services/groups.rs` — hierarchy building, selection expression parser/evaluator, spatial filtering
- `src-tauri/src/database/local/groups.rs` — group CRUD, membership, tag storage (JSON column on `fixture_groups`)
- `src-tauri/src/commands/groups.rs` — Tauri commands for groups and tags
- `src/features/universe/components/grouped-fixture-tree.tsx` — UI for managing groups and assigning tags
- `src/features/universe/components/tag-expression-editor.tsx` — autocomplete editor for tag expressions

---

## Documentation

- [User Guide](https://luma.show/docs/user-guide/why-luma) — Why Luma exists, venues, groups & tags, patterns, annotations, performing
- [Node Reference](https://luma.show/docs/node-reference) — Complete reference for all pattern graph node types
- [Architecture](https://luma.show/docs/architecture/overview) — Signal system, node graph engine, compositor, DMX pipeline, selection system
- [Glossary](https://luma.show/docs/glossary) — Canonical terms used throughout the codebase
