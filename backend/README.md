# Rust backend

`backend/` is the shared Rust library for the native GPUI desktop app and the
headless tools.

- `src/models/`: shared data schemas.
- `src/database/local/`: local SQLite access.
- `src/database/remote/`: direct Supabase PostgREST calls for share codes
  (join a venue, publish a share code, leave a venue), and the HTTP client that
  sync media transfer uses.
- `src/sync/`: PowerSync row sync, the `changes` log triggers and media
  transfer. See [docs/design/sync.md](../docs/design/sync.md).
- `src/services/`: domain logic, persistence and audio analysis.
- `src/preprocessing/`: the track analysis DAG. See its `README.md`.
- `src/eval/`: score evaluation for playback and previews.
- `src/dispatch/`: the command table and host-independent handlers.
- `src/agent/` and `src/agent_execution/`: agent turns, Python workspaces,
  tools and sandbox integration.
- `src/bin/`: headless hosts (`agent_harness`, `luma-mcp`, `luma-record`) and
  developer tools such as `render_venue` and `profile_score_timeline`.
- `crates/`: pattern evaluation, fixture kinematics, DJ protocols, MCP types and
  PowerSync SDK glue.
- `python/`: analysis workers and their dependencies.
- `migrations/`: append-only SQLite migrations. A schema change that syncs also
  needs a file in `supabase/migrations/`.

The desktop entry point is `gpui/crates/app/src/main.rs`. Its library owns the
Tokio runtime and services. Desktop startup prepares Python in the background,
resumes pending analysis, and enables Art-Net output. The headless hosts
(`src/headless_host.rs`) start without Art-Net, audio devices or the playback
loops.

`luma.db` is in the `com.luma.luma` platform config directory.

```sh
cargo +1.97.1 check --manifest-path backend/Cargo.toml --workspace --all-targets
cargo +1.97.1 test --manifest-path backend/Cargo.toml --lib
```
