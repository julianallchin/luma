<p align="center">
  <img src="assets/luma.png" alt="Luma Logo" width="200" />
</p>

<h1 align="center">Luma</h1>

<p align="center">
  <img src="www/public/screenshots/hero.webp" alt="Luma in Arrange mode: moving heads and lasers in haze above a song's score, with the agent chat on the left" />
</p>

<p align="center">
  <a href="https://github.com/julianallchin/luma/actions/workflows/ci.yml">
    <img src="https://github.com/julianallchin/luma/actions/workflows/ci.yml/badge.svg" alt="CI" />
  </a>
</p>

Procedural lighting control for portable light shows that work across any venue. Traditional lighting consoles record hardware-specific DMX instructions that break when you change venues. Luma records *intent* — patterns that automatically adapt to whatever fixtures are in the room.

Think of it like sheet music vs. a recording. Sheet music says "play a C major chord" and works on any instrument. Luma says "red circular pulse" and works on any rig.

## Documentation

- **[User Guide](https://luma.show/docs/user-guide/why-luma)** — How Luma works, the full workflow from venue setup to live performance
- **[Architecture](https://luma.show/docs/architecture/overview)** — Node graph engine, compositor, DMX pipeline, fixture system, database, design decisions
- **[Node Reference](https://luma.show/docs/node-reference)** — Pattern graph nodes, one page per category

## The Workflow

A window is one venue, in one of three modes: **Rig**, **Arrange** and **Live**.
An agent chat sits on the left in each.

1. **Build the rig** (Rig) — Place fixtures and laser projectors in 3D, patch them, and route each universe to an output
2. **Group fixtures** — Name groups of fixtures (`front_wash`, `back_movers`); scores target groups, never fixtures
3. **Import songs** — From Engine DJ, Rekordbox, audio files or a folder; Luma analyzes beats, stems and chords
4. **Arrange a score** (Arrange) — Place clips on a song's timeline; each clip is a small graph that turns audio and beat data into light
5. **Perform** (Live) — Follow a DJ (Rekordbox, Pro DJ Link or StageLinQ), play patterns from pads and faders, bind MIDI and OSC controllers, and output Art-Net or USB DMX

## Project Structure

- **`gpui/`** — Native Rust desktop UI and wgpu stage renderer
- **`backend/`** — Shared Rust backend (SQLite, pattern engine, audio analysis, DMX output, sync)
- **`www/`** — Documentation site ([luma.show](https://luma.show))
- **`resources/fixtures/`** — QLC+ fixture definition library (thousands of fixtures)
- **`harness/`** — Renderer goldens, reference captures and image comparison tools (`cd harness && bun install`)
- **`scripts/`** — Setup, release bundling, headless agent tools (`scripts/headless/`) and a library backup (`scripts/library/snapshot.py`)
- **`deploy/`**, **`supabase/`** — The sync service's rules, the server's migrations, and a disposable Postgres + PowerSync stack for the sync tests (`deploy/test-stack/`)

## Getting Started

You need [rustup](https://rust-lang.org/tools/install/),
[Git LFS](https://git-lfs.com), Python 3, and on macOS Xcode (the Metal shader
compiler). Linux needs a few system libraries; see
[gpui/BUILD.md](gpui/BUILD.md#linux-system-libraries).

```sh
git clone git@github.com:julianallchin/luma.git   # SSH: the one submodule is SSH too
cd luma
scripts/setup              # toolchain, submodule, LFS files, git hooks; checks the rest
python3 gpui/dev.py        # build, launch, and rebuild on save
```

`rust-toolchain.toml` pins Rust 1.97.1 for the whole repository, so plain
`cargo` works from anywhere inside it. Without the watcher:
`cargo run --manifest-path gpui/Cargo.toml -p luma-app`.

The first build downloads a bundled Python 3.12 runtime and ffmpeg into
`backend/`. At first launch the app creates the Python environment for the
analysis workers (beats, stems, chords) in the background.

## Tests

Run from the repository root:

| What | Command |
|---|---|
| Native app, compile everything | `cargo check --manifest-path gpui/Cargo.toml --workspace --all-targets` |
| Backend (library, bins, crates) | `cargo test --manifest-path backend/Cargo.toml --workspace` |
| UI script tests (`*.test.js`) | `gpui/test` (see [gpui/BUILD.md](gpui/BUILD.md#script-tests-gpuitest)) |
| One native crate | `cargo test --manifest-path gpui/Cargo.toml -p luma-render` |
| Agent Python executor | `python3 backend/python/luma_exec/tests/run_tests.py` |
| Sync against a live stack (Docker) | `python3 deploy/test-stack/run.py --test sync::two_device_tests` |

A golden test fails when its file is stale and never rewrites it; regenerate
with `LUMA_UPDATE_GOLDENS=1`, review the diff and commit it. CI
(`.github/workflows/ci.yml`) runs the first two rows, the Python suite, the
Rust headless app tests (`gpui-agent --test headless`) and the scene, pattern
and updater crates.

## Where to read next

- [AGENTS.md](AGENTS.md) — conventions: code philosophy, migrations, tests, UI style
- [gpui/BUILD.md](gpui/BUILD.md) — build profiles, target directories, the test harness
- [backend/README.md](backend/README.md) — the backend's layout
- [docs/README.md](docs/README.md) — an index of how each system works
- [deploy/README.md](deploy/README.md) — deploying Supabase and PowerSync
- [RELEASING.md](RELEASING.md) — tagging a release and the nightly build
