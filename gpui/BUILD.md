# Building the gpui workspace

## Where the time goes

Current, measured on a quiet machine after the 2026-08-23 rebuild:

| invocation | wall |
|---|---|
| `cargo check --workspace --all-targets`, **warm** | **4.3 s** |
| the same, from an empty target dir | 3 m 46 s |
| `cargo test -p gpui-agent --test headless --no-run`, cold | 3 m 31 s |
| `cargo check --workspace --all-targets` in `src-tauri`, cold | 2 m 9 s |

The warm number used to be ~6.5 minutes. Almost none of that was compilation.

### The pathology, so it is recognisable if it returns

Measured 2026-08-22 with ~13 agents sharing this checkout, against a 382 GB
target directory on a 96%-full volume:

| invocation | wall | user | sys |
|---|---|---|---|
| `cargo test -p gpui-agent --test keyboard --no-run` (after `touch`) | 313 s | 10.9 s | 30.0 s |
| same, **no-op** (nothing to rebuild) | 37.9 s | 0.3 s | 0.2 s |
| `cargo check -p gpui-agent --test keyboard` (after `touch`) | 191 s | 5.9 s | 28.9 s |

Read the middle row: a build with **nothing to do** burned 37.9 s of wall for
0.5 s of CPU. That is not compilation, it is blocking on
`target/debug/.cargo-lock`. Cargo takes one exclusive lock per target
directory, so N concurrent agents against one target dir do not share a cache —
they queue. Note also `sys` running 5x `user`: that is the filesystem, because
every fingerprint check had to stat its way through 382 GB.

So when builds feel slow, **measure before optimising, and measure CPU rather
than wall** (`user + sys` is contention-immune; wall is not). The three costs,
in the order they actually mattered, were lock queueing, then filesystem, then
— a distant third — rustc.

Two habits keep it from coming back: one feature set per target directory (see
below), and do not let the trees grow unbounded. 564 GB across the two
workspaces had to be reclaimed to get here.

`cargo check` **does not link**, which is why consolidating the test binaries
(see `docs/build-overhaul.md`) helps `build`/`test --no-run` far more than it
helps `check`.

## Feature-set discipline

Before the rebuild, `target/debug/deps` held **50 copies of
`libluma_app-*.rlib`, 10.9 GB**, all created within two days — not stale
garbage to sweep, but the live working set of every feature permutation anyone
had built. Each distinct feature set is a distinct artifact hash, and flipping
back and forth does not reuse; it accumulates. That is how a target directory
reaches 382 GB.

So: **pick one feature set per target directory and stay in it.** The two trees
exist for exactly this reason and are both cheap to keep (3.9 GB and 3.4 GB
checked); it is the *flipping* that costs, not the second directory.

```sh
# Headless iteration — the default. Do not pass --features here.
cargo check -p gpui-agent --all-targets

# Pixel runs get their own tree, so flipping never evicts the headless one.
# Set this once per shell; every pixel command below assumes it.
export PIXEL_TARGET="$(git rev-parse --show-toplevel)/gpui/target-pixel"
CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel
```

> **`CARGO_TARGET_DIR` must be absolute.** Cargo resolves a relative value
> against the *current working directory*, not the workspace root — so
> `CARGO_TARGET_DIR=target-pixel` run from `crates/agent/tests/` silently
> builds a whole second tree at `crates/agent/tests/target-pixel/`. That
> happened, and cost 6.8 GB before anyone noticed. `git rev-parse` above keeps
> it anchored to the repo from any directory; `.gitignore` also matches
> `target-pixel/` at any depth so a stray one at least never gets committed.

## Test suites

`crates/agent/tests/` is five grouped binaries plus two that are deliberately
their own — 49 test targets became 7. Filter by test name to run what used to
be one file:

```sh
cargo test --test headless tab_chrome          # was --test tab_chrome
cargo test --test unit                         # pump, interpreter, MCP
CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test --features pixel --test app_pixel
CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test --features pixel --test ui_pixel
```

| target | what is in it |
|---|---|
| `headless` | outside-in app tests, no GPU |
| `chat` | the agent panel — see below |
| `app_pixel` | outside-in app tests against a real renderer |
| `ui_pixel` | `luma-ui` surfaces under a renderer, no app |
| `unit` | the harness's own tests — no library, no renderer |
| `pixel_suite_guard` | ungated on purpose; keep it standalone |
| `visualizer_playback_zoom_repro` | a diagnostic, deliberately standalone |

Adding a test file means adding one `mod` line to that group's `main.rs`.

The two standalone targets are standalone for reasons, not by omission.
`pixel_suite_guard` must stay ungated so a feature-off run has something to
say. `visualizer_playback_zoom_repro` is an instrument rather than a gate — it
reports an attribution and is read with `--nocapture` — and keeping it out of
`app_pixel` means whoever is iterating on playback links a small binary
instead of eighteen files' worth.

### These tests are wall-clock bound, and that constrains grouping

They poll for a rendered frame with a timeout and hold fixture responses for a
fixed number of milliseconds, so a test starved of CPU misses a deadline and
fails an assertion unrelated to what it was testing.

How much that bites depends entirely on the machine. On the pathological one —
thirteen agents on a shared target directory, 96%-full disk — a single test
took 60–120 s *by itself*, and consolidating put 32 of them on 16 cores and
started failing a different one every run. On a quiet machine with a clean
disk the same 33 tests finish in **28 s** and pass whether capped or not.

`Harness::headless` caps concurrent driving threads (`HARNESS_CONCURRENCY` in
`crates/agent/src/lib.rs`). Keep it, but know what it is: **insurance for a
loaded machine, not the thing that makes the suite green.** Capped and uncapped
are within noise on a healthy one. The clean disk did the real work.

`chat` is its own target one step further along the same axis: it streams a
reply at a fixed cadence and asserts the transcript grew between frames, so it
wants CPU on the schedule that cadence assumes.

Two rules follow. Group by what a test needs from the *process*, not by
subject matter. And when a consolidated suite goes red, run the test alone
before believing the diff — these fail for reasons that have nothing to do
with the change in front of you.

> **Known: `headless` is intermittently red** — about one run in three is
> clean, and the loser varies. It is an order-dependent state leak between
> tests, not timing, and it has a deterministic repro. See the OPEN section at
> the top of `docs/build-overhaul.md` before spending time on it.

For a whole pixel session, export `CARGO_TARGET_DIR="$PIXEL_TARGET"` once
rather than prefixing each command — same absolute-path rule as above.

The tradeoff is disk. It is affordable now — 632 GB free, and the two trees are
a few GB each checked — but that is only true because they were just rebuilt
from empty. Watch the number before adding a third variant.

## What is already optimal — don't re-litigate

- **Linker.** Xcode 26.1, `ld-1230.1`. The parallel linker has been the default
  since Xcode 15; `-C link-arg=-ld_new` is a no-op here and is not worth adding.
- **`split-debuginfo`.** Cargo already defaults to `unpacked` on macOS, so DWARF
  stays in `.o` files and the linker writes a debug map rather than copying
  sections. There is no packed-to-unpacked win left to take.
- **`[profile.dev.package."*"] opt-level = 2`** in `Cargo.toml` is load-bearing:
  gpui is unusably slow at opt-level 0. Leave it.

## sccache

Not installed. It is the right answer to the per-target-dir duplication above —
it would make multiple target dirs nearly free by caching at the crate level
instead of the tree level — but installing a system-wide toolchain wrapper is a
decision for a human, not something to slip into a build config. If you want
it: `brew install sccache`, then add to `gpui/.cargo/config.toml`:

```toml
[build]
rustc-wrapper = "sccache"
```

Note that this invalidates the whole tree the first time (see below).

## The dev build is the build people judge the renderer by

`[profile.dev.package."*"]` reaches dependencies only, so for a long time every
workspace member compiled at `opt-level = 0` while everything under them ran at
2. For the per-frame CPU path that is not a mild tax. Measured on a 480-fixture
stage with a track playing, in the harness at a quarter-screen window:

| | `opt-level = 0` | `opt-level = 2` |
|---|---|---|
| frame assembly (`BUILD`) | 19.1 ms | 1.0 ms |
| whole UI-thread half (`UI`) | 21.6 ms | 3.1 ms |
| gpui element walk (`drawMs` p50) | 23.6 ms | 6.2 ms |

That is 28 fps versus vsync, from a build flag. `luma-render` and `luma-scene`
now carry their own `opt-level = 2` for this reason; see the comment in
`Cargo.toml`.

The trap this closes is not the slowness, it is the *disagreement*.
`profile-volumetrics` is documented to run `--release`
(`crates/render/src/bin/profile-volumetrics.rs:419`), so it kept reporting
budgets comfortably met while the app in front of a human could not hold them —
two honest measurements of two different binaries. **When a profiler and a pair
of hands disagree about the same renderer, check that they are the same build
before believing either.**

## Any change to `[profile.*]` is a stop-the-world event

Profile settings are part of every artifact fingerprint, so editing
`[profile.dev]` invalidates the whole tree and forces every concurrent agent
into a cold rebuild, serialized behind the one lock. Schedule these for a quiet
window; never land one mid-session. The debuginfo change now in
`gpui/Cargo.toml` was landed exactly that way — see `docs/build-overhaul.md`
for what it does and why.
