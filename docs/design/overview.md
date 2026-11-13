# Architecture Overview

Luma is a native Rust desktop app. The UI uses GPUI. The shared backend owns
persistence, audio analysis, pattern evaluation, agents, sync and lighting
output. The documentation website in `www/` is a separate Next.js application.

## Tech stack

- **Native UI**: GPUI, gpui-component and the shared controls in `gpui/crates/ui`.
- **Rendering**: wgpu, with a shared scene model and fixture kinematics.
- **Backend**: Rust, Tokio, SQLite through sqlx, ffmpeg for decoding and CPAL for playback.
- **Analysis**: Python workers for beats, stems, chords, MERT features, drum onsets, bar classifications and genres.
- **Networking**: PowerSync and Supabase for sync, StageLinQ, Pro DJ Link and rekordbox (through rkbx_link over OSC) for DJ equipment, MIDI and OSC for live control, and Art-Net and USB DMX interfaces for DMX output.

## Data

Scores describe lighting without physical fixtures. A selection references
groups of the score's venue. The venue maps those groups to its fixtures.

The library database is `luma.db` in the platform app config directory:

- macOS: `~/Library/Application Support/com.luma.luma/luma.db`
- Linux: `~/.config/com.luma.luma/luma.db`
- Windows: `%APPDATA%\com.luma.luma\luma.db`

Venues, tracks, groups, patterns and scores are rows in this database. There
is no per-venue project file. The auth session is in a separate `state.db` in
the same directory. See [Database](database.md).

## Repository organization

```text
gpui/
  crates/app/       The window shell (Rig, Arrange and Live modes), editors and desktop lifecycle
  crates/ui/        Shared controls, themes and embedded icons
  crates/chat/      Agent conversation UI
  crates/md/        Markdown rendering
  crates/scene/     Shared scene model and spatial geometry
  crates/render/    wgpu renderer
  crates/agent/     Native automation and UI test harness
  crates/update/    App updates
backend/
  src/models/       Shared Rust schemas
  src/database/     Local SQLite and remote database access
  src/services/     Domain operations
  src/dispatch/     Command table and handlers
  src/eval/         Scene compilation and compositing
  src/output/       DMX output: Art-Net and USB
  src/inputs/       MIDI and OSC inputs, controllers, bindings and pad lights
  src/controls/     Each venue's bank of live controls
  src/perform/      DJ deck sources and the Perform set
  src/laser/        Laser projectors and the laser solver
  src/sync/         PowerSync row sync and media transfer
  src/preprocessing/  Audio-analysis scheduling
  src/agent_execution/  Python workspaces and tools
  src/bin/luma-cli/  Luma without a window: rpc, agent, mcp, record, render, profile
  crates/patterns/  Pattern graph engine (luma-patterns)
  crates/fixture-kinematics/  Fixture cell positions and beam rays
  src/rekordbox/    Rekordbox library reads, via the rbox crate
  src/engine_dj/    Engine DJ library reads
  crates/stagelinq/, crates/prodjlink/  DJ integrations
  crates/sync/      PowerSync SDK glue
  python/           Analysis workers
  migrations/       Append-only SQLite migrations
resources/          Fixture definitions, meshes and bundled skills
supabase/           Remote database migrations
deploy/             The sync service's rules and a disposable sync test stack
scripts/            Setup, release bundling, headless tools, screenshots, library backup
harness/            Renderer references and comparison tools
www/                Documentation website
```

The native app calls the backend through `luma_lib::dispatch`. Events carry
progress back to GPUI. The desktop host enables DMX output (Art-Net and USB) and the MIDI and OSC inputs. Headless test
hosts do not bind sockets.

The `gpui/` and `backend/` Rust workspaces are separate. See `gpui/BUILD.md`
for build and test commands.
