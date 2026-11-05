# Building the gpui workspace

## Prerequisites

- **Rust 1.97.1.** The repository's `rust-toolchain.toml` pins it for both
  workspaces, and rustup installs it on the first `cargo` run inside the
  repository.
- **Git LFS.** The n2n drum-onset weights (`backend/python/n2n/weights.pt`) are
  an LFS file. The hooks in `.githooks/` call `git-lfs` and stop if it is
  missing. Enable them with `git config core.hooksPath .githooks`.
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

### USB DMX dongles

Luma opens USB DMX dongles (SoundSwitch Micro and Control One) directly, with
no system library. On Linux a normal user cannot open them until a udev rule
grants access:

```sh
sudo cp resources/linux/70-luma-dmx.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Unplug and replug the dongle. Without the rule, the Outputs panel shows the
dongle with a permission message.

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

When a profile and the running app disagree about the renderer, first check
that they are the same build.

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
Do not let target directories grow without limit. `cargo clean -p luma` in
`backend/` and `gpui/` drops the biggest stale artifacts and keeps the
dependencies, so the next build only redoes our own crates.

Do not point two worktrees at one `CARGO_TARGET_DIR`. Cargo hashes path
packages by their location in the workspace, so both worktrees write to the
same artifacts, and a build script's output can name files in the other
worktree, or in one that has been deleted.

When every render or render test is slow (more than 10 s before the first
frame), the GPU driver is compiling all the pipelines again. NVIDIA's
on-disk shader cache is capped at 1 GB and never evicted. Each rebuild is a
new binary to the driver, so the cache fills up and then stops saving
anything. The root `.cargo/config.toml` and `gpui/dev.py` raise the cap
through `__GL_SHADER_DISK_CACHE_SIZE`. A process started some other way does
not get that setting. If renders slow down again, clear
`~/.cache/nvidia/GLCache`.

`cargo check` does not link. Fewer test binaries help `build` and
`test --no-run` more than they help `check`.

The linker and `split-debuginfo` defaults on macOS are already the fast options.
Do not add `-ld_new` or change `split-debuginfo`.

sccache is not installed. It would make several target directories cheap. It is
a system-wide tool, so a human must decide to install it. To use it, install it
and add `rustc-wrapper = "sccache"` under `[build]` in `gpui/.cargo/config.toml`.
This invalidates the whole tree once.

## Test suites

Keep integration tests in as few binaries as possible, because each
binary links the whole crate graph. `crates/agent/tests/` has 5 test targets,
each a directory that groups many test files. Filter by test name to run one
file:

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
| `app_pixel` | outside-in app tests with a real renderer; `pixel_suite_guard` in it is not feature-gated, so a run without `pixel` still reports something |
| `ui_pixel` | `luma-ui` surfaces with a renderer, no app |
| `unit` | tests of the harness itself: no library, no renderer |

Two measurement tools for a copy of a real library (`LUMA_REAL_CONFIG`) are
examples, not tests: `cargo run -p gpui-agent --features pixel --example
track_editor_real_budget` (frame cost while the editor moves) and `--example
visualizer_real_score_window` (a real score through the renderer, second by
second).

`luma-render` has one binary, `--test render`, plus four that change
process-wide state and so stay alone: `hdr_dither`, `hdr_presentation`,
`shared_presentation` and `waveform`. `luma-ui` has one, `--test ui`.

To add a test file to a group, add one `mod` line to that group's `main.rs`.
Members use `use super::support;`. Do not declare a second `mod support;`,
because that compiles a second copy of the shared support state.

### Script tests (`gpui/test`)

A UI test can also be a `.test.js` file under `crates/agent/tests/js/`. A new or
changed test file needs no Rust build. From the repository root:

```sh
gpui/test                        # every headless file
gpui/test settings slider        # files by name, path or glob
gpui/test --filter "model picker" --slowest 5
gpui/test --pixel                # files under tests/js/pixel, in target-pixel
gpui/test --help
```

Each test gets its own seeded library and its own app. `-j N` sets how many run
at once (default: half the cores; 2 with `--pixel`). A failure prints the
message, the file line, the last frame's nodes and the console output.
`--json` prints one JSON object per test. `--watch` runs a file again when it
changes.

A file calls `fixture({...})` once, with the options of
`gpui_agent::fixture::Fixture` (`track: false`, `seconds`, `clips`, `rig`,
`extra_scores`, `model`, `tools`, …), then `test(name, fn)` for each test.
`model` replays the agent's turns (see `src/fixture/agent.rs`; example:
`tests/js/headless/chat.test.js`). `expect`, `assert`,
`nav`, `until` and the `app` API are available. `src/api.d.ts` declares `app`
and `image`. `src/runner.js` declares the rest.

`until(what, pred, { timeoutMs })` fails after 5 s by default. Between checks
it asks for a frame only when the app did something.

### Timing and grouping

These tests are bound by wall-clock time. They wait for a rendered frame with a
timeout, and fixtures hold responses for a fixed time. A test that does not get
CPU misses a deadline and fails an unrelated assertion.

`Harness::headless` limits the number of driving threads
(`HARNESS_CONCURRENCY` in `crates/agent/src/lib.rs`, 6 by default; `luma-test`
sets it to its `-j`). This limit protects a loaded machine. On a quiet machine,
results with and without the limit are the same.

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
`STAGE_IMAGE_ID` (`crates/app/src/visualizer.rs`) and the glass `GENERATION`
counter (`crates/ui/src/glass.rs`). The `HOVER_FADES` thread-local this list
used to name is gone: hover fades live in each element's state now. Changing `HARNESS_CONCURRENCY` does not fix it.

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

## Gotchas

Collected from past work in this tree. Each cost someone an afternoon.

**Builds and tests**

- `cargo test` stops at the first failing test binary. Pass `--no-fail-fast`
  when a run should report every failure, as a golden run should.
- An integration test build also builds the crate's binaries, so
  `cargo test -p luma-render` relinks `target/debug/render-goldens`. Copy a binary
  you are comparing against before running tests.
- Cargo rebuilds by mtime. A file restored from a backup keeps its old mtime,
  and the build silently keeps the stale artifact. `touch` it.
- Build `luma-mcp` with `--bin luma-mcp` from `backend/`. It is a binary of
  the `luma` package, not a package.
- A worktree needs `git submodule update --init` before the backend compiles:
  `backend/src/runtime/python.rs` embeds the consonance-ACE requirements file.
  Give each worktree its own absolute `CARGO_TARGET_DIR` or its own `target/`.

**Measuring**

- The GPU's clock ramps. The first run after the app quits reads high, and any
  probe under about 50 runs is measuring the clock governor. Throw away the
  first run, sweep sizes in both directions, and cool down between runs.
- macOS `sample` over a whole harness run attributes startup and preview
  renders to the same symbols as live frames. Sample `profile-score`
  instead. A release-build sample needs `dsymutil` for file:line.
- `luma-record`'s mp4 is not byte-reproducible. Compare decoded frames.

**Renderer**

- `MSAA_SAMPLES` (`crates/render/src/gpu/mod.rs`) cannot simply be 2: Metal
  refuses 2 samples on `Rgba16Float` unless the device requests
  `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`.
- wgpu-hal never turns off Metal's fast math, so WGSL cannot promise a float
  operation order. Exactness evidence for a rewrite is a CPU mirror test plus
  pixels.
- naga's MSL backend never emits `[[early_fragment_tests]]`: fragment atomics
  run before the depth test.
- One buffer cannot be a read-write storage binding and the
  `dispatch_workgroups_indirect` source in the same dispatch.

**Data and tools**

- Open a live `luma.db` (WAL) with `sqlite3` by its plain path. `?mode=ro`
  has failed on it.
- Homebrew's Python 3.14 has shipped a broken `pyexpat`. Use 3.12 for tools.
- zsh does not word-split an unquoted `$var`. Use an array, or `xargs`.
