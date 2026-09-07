# Rust backend

`backend/` is the shared Rust library used by the native GPUI desktop and CLI
tools. It has no Tauri or webview dependency.

- `src/models/`: shared data schemas.
- `src/database/`: local SQLite and remote Postgres access.
- `src/services/`: domain logic, persistence, audio analysis, and authored history.
- `src/dispatch/`: the command table and host-independent handlers.
- `src/agent_execution/`: Python workspaces, tools, and sandbox integration.
- `crates/`: pattern evaluation, fixture kinematics, DJ protocols, and MCP types.
- `python/`: analysis workers and their dependencies.
- `migrations/`: append-only SQLite migrations, paired with `supabase/migrations/`.

The desktop entry point is `gpui/crates/app/src/main.rs`. Its library owns the
Tokio runtime and services. Desktop startup prepares Python in the background,
resumes pending analysis, and enables Art-Net output. Harness startup keeps
those physical and installation side effects disabled.

The global `luma.db` remains in the existing `com.luma.luma` platform config
directory. The folder rename does not relocate user data or change migrations.

```sh
cargo +1.97.1 check --manifest-path backend/Cargo.toml --workspace --all-targets
cargo +1.97.1 test --manifest-path backend/Cargo.toml --lib
```

Rust models retain `ts-rs` metadata; there is no desktop TypeScript frontend.
