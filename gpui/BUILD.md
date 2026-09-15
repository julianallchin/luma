# Building the gpui workspace

## Prerequisites

- **Rust 1.97.1.** `gpui/rust-toolchain.toml` pins it. From the repository
  root, pass `+1.97.1` to cargo.
- **Git LFS.** The n2n drum-onset weights (`backend/python/n2n/weights.pt`) and
  the backend golden fixtures (`backend/tests/golden/`) are LFS files. The hooks
  in `.githooks/` call `git-lfs` and stop if it is missing. Enable them with
  `git config core.hooksPath .githooks`.
- **Submodules.** Run `git submodule update --init --recursive`. The chord
  analysis worker needs `backend/python/consonance-ACE`.
- **Network on the first build.** `backend/build.rs` downloads a standalone
  Python 3.12 runtime into `backend/python-runtime/` and ffmpeg into
  `backend/ffmpeg-runtime/`. The build does not need a system Python. If a
  download fails, the build continues with a warning, and analysis that needs
  that runtime does not work.
- **Python 3** for the watcher `gpui/dev.py`.
- **macOS:** Xcode. The vendored gpui compiles its Metal shaders with
  `xcrun metal` at build time.
- **Linux:** the system libraries below.

## Linux system libraries

CI installs these on Ubuntu (`.github/workflows/ci.yml`):

```sh
sudo apt-get install -y libasound2-dev libxkbcommon-x11-dev libwayland-dev \
  libvulkan-dev libfontconfig1-dev libssl-dev libclang-dev libx11-xcb-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev
```

A missing library shows up only at link time, as
`rust-lld: error: unable to find library -l<name>`. The `-dev` package carries
the `.so` symlink that the linker needs.

## Rebuild on save

From the repository root, run:

```sh
python3 gpui/dev.py
```

The watcher builds and launches `luma-app`. It rebuilds after source or asset
changes in `gpui/`, `backend/`, and `resources/`. It includes new untracked
files. It ignores Git-ignored outputs and Markdown files. Saves are debounced.
A change during a build causes another build before launch.

The running app stays open during a build and on a build error. After a
successful build, the watcher stops its app and launches the new executable.
This is a full restart: playback and editing state are lost. Close other Luma
instances first; the watcher only manages the process it starts. Ctrl-C stops
the watcher and its app. The watcher uses Cargo's target-directory setting.

## Build profiles

`gpui/Cargo.toml` sets these profiles:

- `[profile.dev] debug = "line-tables-only"`. Backtraces through Luma crates
  keep `file:line`.
- `[profile.dev.package."*"] opt-level = 2, debug = false`. `"*"` matches
  dependencies only. gpui is too slow at `opt-level = 0`. Dependency frames keep
  their symbol names but lose line numbers. Dropping dependency debuginfo made
  `deps/` about 60% smaller in a measurement.
- `luma-render` and `luma-scene` have `opt-level = 2`. `"*"` does not reach
  workspace members, and the per-frame CPU path is slow at `opt-level = 0`. On
  a 480-fixture stage with a track playing, frame assembly took 19.1 ms at
  level 0 and 1.0 ms at level 2.
- `objc2` has `debug-assertions = false`. Its selector checks took most of the
  Metal command-encoder time in a dev build.

`luma-app` stays at `opt-level = 0` so that a debugger works well there.

`profile-volumetrics` refuses a debug build
(`crates/render/src/bin/profile-volumetrics.rs:150-153`). When a profiler and
the running app disagree about the renderer, first check that they are the same
build.

Any change to `[profile.*]` changes every artifact fingerprint. It forces a
cold rebuild for every agent that shares the target directory, one at a time
behind the Cargo lock. Make such a change only when nobody else is building.

## Target directories and feature sets

Each feature set produces different artifacts. Changing feature sets in one
target directory does not reuse the old artifacts; it adds new ones. Use one
feature set per target directory.

```sh
# Headless work: the default tree. Do not pass --features.
cargo check -p gpui-agent --all-targets

# Pixel work: a separate tree. Set this once per shell.
export PIXEL_TARGET="$(git rev-parse --show-toplevel)/gpui/target-pixel"
CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel
```

`CARGO_TARGET_DIR` must be an absolute path. Cargo resolves a relative value
against the current directory, not the workspace root. `.gitignore` matches
`target-pixel/` at any depth.

## When builds are slow

Measure CPU time (`user + sys`), not wall time. Wall time includes waits.
Cargo holds one exclusive lock per target directory, so concurrent builds in one
directory wait for each other. A no-op build with a high wall time and near-zero
CPU is waiting on `target/debug/.cargo-lock`. A large target directory also
makes `sys` time high, because every fingerprint check reads the file system.
Do not let target directories grow without limit.

`cargo check` does not link. Fewer test binaries help `build` and
`test --no-run` more than they help `check`.

The linker and `split-debuginfo` defaults on macOS are already the fast options.
Do not add `-ld_new` or change `split-debuginfo`.

sccache is not installed. It would make several target directories cheap. It is
a system-wide tool, so a human must decide to install it. To use it, install it
and add `rustc-wrapper = "sccache"` under `[build]` in `gpui/.cargo/config.toml`.
This invalidates the whole tree once.

## Test suites

`crates/agent/tests/` has 9 test targets. Five are directories that group many
test files. Four are single files. Filter by test name to run one file:

```sh
cargo test -p gpui-agent --test headless tab_chrome
cargo test -p gpui-agent --test unit
CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel --test app_pixel
CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel --test ui_pixel
```

| target | contents |
|---|---|
| `headless` | outside-in app tests, no GPU |
| `chat` | the agent panel |
| `app_pixel` | outside-in app tests with a real renderer |
| `ui_pixel` | `luma-ui` surfaces with a renderer, no app |
| `unit` | tests of the harness itself: no library, no renderer |
| `pixel_suite_guard` | not feature-gated, so a run without `pixel` still reports something |
| `visualizer_playback_zoom_repro` | a diagnostic; it reports and does not assert |
| `track_editor_real_budget` | `#[ignore]`; frame cost on a copy of a real library |
| `visualizer_real_score_window` | `#[ignore]`; a real score through the renderer, second by second |

To add a test file to a group, add one `mod` line to that group's `main.rs`.
Members use `use super::support;`. Do not declare a second `mod support;`,
because that compiles a second copy of the shared support state.

### Timing and grouping

These tests are bound by wall-clock time. They wait for a rendered frame with a
timeout, and fixtures hold responses for a fixed time. A test that does not get
CPU misses a deadline and fails an unrelated assertion.

`Harness::headless` limits the number of driving threads
(`HARNESS_CONCURRENCY` in `crates/agent/src/lib.rs`). This limit protects a
loaded machine. On a quiet machine, results with and without the limit are the
same.

`chat` is a separate target because it streams a reply at a fixed rate and
checks that the transcript grows between frames.

Two rules follow:

- Group tests by what they need from the process, not by subject.
- When a grouped suite fails, run the failing test alone before you trust the
  failure.

`headless/library_foundation.rs` and `headless/add_tracks_flow.rs` still call
`std::env::set_var` for `LUMA_CONFIG_DIR` and `LUMA_CACHE_DIR`. They drive
`Library` on the test thread. If one of these files adds a parallel test, the
tests can race on those variables.

### Known issue: order-dependent failures in `headless`

Last confirmed on 2026-08-23. Not checked since. `headless` failed
intermittently. The cause was state left in the process by an earlier test,
not timing: more serial runs failed more often. This command failed
`track_editor`, `track_editor_ux` and `track_editor_waveform` every time, and
each passed when run alone:

```sh
cargo test -p gpui-agent --no-fail-fast --test headless track_editor -- --test-threads=1
```

Look for process-wide state that a new `Fixture` does not reset, for example
`STAGE_IMAGE_ID` (`crates/app/src/visualizer.rs`), the glass `GENERATION`
counter (`crates/ui/src/glass.rs`) and the `HOVER_FADES` thread-local
(`crates/ui/src/motion.rs`). Changing `HARNESS_CONCURRENCY` does not fix it.

## Measuring frame cost in the harness

- `app.timings()` returns the CPU half of a frame: the scene build only.
- `app.frames()` builds scenes but does not rasterize. Only `app.screenshot()`
  reaches the GPU. Wall time around a frame pump does not measure the renderer.
- On macOS, `LUMA_FILTER_PROFILE=1` prints per-frame GPU times for content
  filter passes (`vendor/zed/crates/gpui_apple/src/metal_renderer.rs`).
- A route's content is built again on every frame the window draws. A settled
  dialog draws no frames, but a route morph, a scrim fade or a hover fade does.
  `float::menu_row` puts a hover fade on every row.
- Virtualize long lists with `uniform_list`, or accept the cost. Do not compute
  a row set on every frame. Cache it and compute it again only when its inputs
  change, as `add_tracks.rs` (`browser_shown`) and `tracks.rs` (`refilter`) do.
- Under machine contention every timing is several times larger. Use the
  minimum of several runs, not the mean.
