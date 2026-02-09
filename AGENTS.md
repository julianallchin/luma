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

---

## Documentation

For conceptual design, architecture, and usage documentation, see:

- [User Guide](docs/user-guide.md) — How Luma works, full workflow from venue setup to live performance
- [Developer Guide](docs/developer-guide.md) — Architecture, signal system, compositor, DMX pipeline
- [Node Reference](docs/node-reference.md) — Complete reference for all pattern graph node types
- [Terminology](docs/terminology.md) — Canonical terms used throughout the codebase
