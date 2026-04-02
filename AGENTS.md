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

## Code Philosophy

Delete dead code — don't comment it out, don't keep it "just in case". If something is being replaced, remove the old thing entirely. No backwards-compatibility shims unless there's a concrete reason (e.g. a migration that must stay). When changing something fundamental, change it all the way.

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

## UI Conventions

- **Confirmation dialogs**: Use the `AlertDialog` component from `@/shared/components/ui/alert-dialog` for destructive confirmations (delete, discard, etc.). Do **not** use the native Tauri `ask()` dialog from `@tauri-apps/plugin-dialog`.

## Version Bumps & Releases

When bumping the version, update **all three files** together: `package.json`, `src-tauri/tauri.conf.json`, and `src-tauri/Cargo.toml`. They can drift out of sync if any one is missed.

To trigger a production release build, push a version tag after committing:

```
git tag v0.x.y && git push origin v0.x.y
```

Pushing to `main` alone does **not** trigger a build — the tag is required.

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

- `src-tauri/src/models/groups.rs` — `FixtureGroup`, name normalization/validation helpers
- `src-tauri/src/services/groups.rs` — hierarchy building, selection expression parser/evaluator, spatial filtering
- `src-tauri/src/database/local/groups.rs` — group CRUD, membership
- `src-tauri/src/commands/groups.rs` — Tauri commands for groups
- `src/features/universe/components/grouped-fixture-tree.tsx` — UI for managing groups
- `src/features/universe/components/group-expression-editor.tsx` — autocomplete editor for group selection expressions

---

## Documentation

- [User Guide](https://luma.show/docs/user-guide/why-luma) — Why Luma exists, venues, groups & tags, patterns, annotations, performing
- [Node Reference](https://luma.show/docs/node-reference) — Complete reference for all pattern graph node types
- [Architecture](https://luma.show/docs/architecture/overview) — Signal system, node graph engine, compositor, DMX pipeline, selection system
- [Glossary](https://luma.show/docs/glossary) — Canonical terms used throughout the codebase
