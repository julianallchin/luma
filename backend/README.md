# Rust backend

`backend/` is the shared Rust library for the native GPUI desktop app and the
headless tools.

- `src/models/`: shared data schemas.
- `src/database/local/`: local SQLite access.
- `src/sync/`: PowerSync row sync, the `changes` log triggers, media
  transfer, and the Supabase Storage and PostgREST clients (`sync/supabase.rs`). See [docs/design/sync.md](../docs/design/sync.md).
- `src/services/`: domain logic, persistence and audio analysis.
- `src/preprocessing/`: the track analysis DAG. See its `README.md`.
- `src/eval/`: score evaluation for playback and previews.
- `src/dispatch/`: the command table and host-independent handlers.
- `src/agent/` and `src/agent_execution/`: agent turns, Python workspaces,
  tools and sandbox integration.
- `src/bin/luma-cli/`: Luma without a window. `luma-cli rpc`, `agent`, `mcp`,
  `record`, `render` and `profile`; `luma-cli --help` lists them.
- `crates/`: pattern evaluation, fixture kinematics, DJ protocols, MCP types and
  PowerSync SDK glue.
- `python/`: analysis workers and their dependencies.
- `migrations/`: append-only SQLite migrations. A schema change that syncs also
  needs a file in `supabase/migrations/`.

The desktop entry point is `gpui/crates/app/src/main.rs`. Its library owns the
Tokio runtime and services. Every host opens a library through `src/boot.rs`,
which owns the startup order: migrations, write admission, services,
replication and devices as asked, persisted settings, and recovery of agent
rows a quit left in progress. Desktop startup then prepares Python in the
background, resumes pending analysis, and runs Art-Net output. The headless
hosts (`src/headless_host.rs`) boot without replication, Art-Net, audio
devices or the playback loops.

`luma.db` is in the `com.luma.luma` platform config directory.

```sh
cargo check --manifest-path backend/Cargo.toml --workspace --all-targets
cargo test --manifest-path backend/Cargo.toml --lib
```
