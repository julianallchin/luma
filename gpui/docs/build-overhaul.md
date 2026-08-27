# Build overhaul

Measurements and conventions live in `../BUILD.md`. This is the plan for the
two changes that cannot be landed casually: the dev profile (stop-the-world)
and the test-binary consolidation (needs source fixes first).

## 1. Dev profile — proposed, NOT applied

```toml
[profile.dev]
debug = "line-tables-only"

[profile.dev.package."*"]
opt-level = 2
debug = false
```

`"*"` matches dependencies but not workspace members, so this keeps `file:line`
in backtraces through Luma's own crates and drops DWARF entirely for the
vendored gpui tree and the rest of the dependency graph. Symbol names survive
(the symbol table is not DWARF), so a panic inside gpui still names its frames;
it just loses line numbers.

Why it is worth doing: the two target trees are 564 GB together (382 GB for
`gpui`, 182 GB for `src-tauri`) on a volume with 87 GB free, and `sys` time is
5x `user` on every measured invocation. The tree is the bottleneck, and full
DWARF on every dependency is what makes it that size.

**Measured, not estimated: 60% off `deps/`.** Built a throwaway crate over the
`regex` graph twice, holding `opt-level = 2` on dependencies in both so the
delta isolates debuginfo alone: 75 MB today, 30 MB with the block above. The
same probe confirms cargo 1.97 accepts every key here without a warning, which
is worth knowing before a change that invalidates 564 GB.

So the tree should come back from the clean roughly 60% smaller than it would
otherwise — regrowing toward ~225 GB rather than ~564 GB, on top of whatever
the test-binary consolidation in §2 takes off.

**Why it is not applied yet.** Profile settings are part of every artifact
fingerprint. Landing this invalidates all 377 GB at once and forces every
concurrent agent into a cold rebuild, serialized behind the single target-dir
lock. It needs a window where nobody else is building. The same window should
be used to `cargo clean` (there is no stale subset to prune — every byte in
`deps/` was written within the last two days) and to establish `target-pixel/`.

Not proposed: linker flags and `split-debuginfo` (already optimal — see
`BUILD.md`), and lowering `opt-level` on dependencies (gpui is unusably slow at
0, which is why the existing override exists).

## OPEN: the headless suite is intermittently red, and it is not timing

Consolidation exposed an **order-dependent state leak between tests** that
per-file process isolation was hiding. It is not caused by the fold-in itself,
by the `Runtime` change, or by the render `opt-level` change (all three were
tested and cleared — see below). It is a latent dependency in the tests that
only became reachable once they shared a process.

**Deterministic repro — use this, not the full suite:**

```sh
cargo test -p gpui-agent --no-fail-fast --test headless track_editor -- --test-threads=1
```

That fails `track_editor`, `track_editor_ux` and `track_editor_waveform` every
time. Each of the three **passes when run alone**, so the state that breaks
them is left behind by a test that ran earlier in the same process.

**The evidence that rules out starvation.** If these were losing a race for
CPU, less concurrency would help. It does the opposite:

| configuration | headless failures |
|---|---|
| `HARNESS_CONCURRENCY = 6` (current) | 0–1, victim varies |
| `HARNESS_CONCURRENCY = 2` | 1–3, the trio consistently |
| `--test-threads=1` (fully serial) | 3, the trio, every time |
| each test alone | none, ever |

More serialization means *more* failures, which is the signature of an
ordering dependency rather than a deadline miss. Do not try to fix this by
tuning the cap; that was measured and it makes things worse.

**Also ruled out:** the render `opt-level = 2` change (failure count is the
same with it reverted — an early 3-vs-1 reading was noise across two
single-sample runs), and fixture-name collision in `Fixture::seed`'s
`remove_dir_all` (all names in the group are unique — checked).

**What the failure actually says.** `track_editor_ux.rs:753` asserts
`end > 1.` — its own guard for "the clip I am measuring is on screen" — and
gets `0s then 0s`, meaning the node was not found at all. So the timeline is
not in the state the script assumes by the time it reads, and the guard is
doing its job.

**Where to look next:** what a preceding test leaves in the process that a
fresh `Fixture` does not reset. The `Runtime` knobs are per-pump-thread and
clear, so the candidates are elsewhere — `STAGE_IMAGE_ID`
(`crates/app/src/visualizer.rs:307`), the glass `GENERATION` counter
(`crates/ui/src/glass.rs:84`), the `HOVER_FADES` thread-local
(`crates/ui/src/motion.rs:637`), or state inside `Library`/the audio host that
outlives an app. Bisect by adding tests to the `--test-threads=1` run until the
trio goes red.

The full suite passes roughly one run in three; the other two lose one test,
usually `add_tracks_flow`, `graph`, or one of the trio.

## 2. Test-binary consolidation — **done**

Landed as described below, with two findings the plan did not anticipate. All
111 tests still exist; 48 test targets became 16, of which 10 are the
`visualizer*` / `tracks` / `track_editor_budget` files held back while the
renderer work is in flight, plus `pixel_suite_guard`. The 37 files that moved
collapsed into five: `headless`, `chat`, `app_pixel`, `ui_pixel`, `unit`.

**Finding 1: the env vars were worse than surveyed.** `LUMA_MOTION_SCALE` was
cached in a `OnceLock`, and `LUMA_STAGE_GPU` / `LUMA_MOTION` were sticky — the
first writer in a process decided for every later one. Rather than fix five
variables five ways, they are now one `luma_ui::runtime::Runtime` carried in
`pump::Config` and installed on the pump thread. The environment remains the
fallback, so a human running one test by hand still has the escape hatches and
every un-migrated file kept working unchanged.

**Finding 2: per-file binaries were silently capping parallelism.** Cargo runs
test binaries one at a time, so only a file's own handful of tests ever
overlapped. These tests are wall-clock bound — 60–120 s each *alone* — and
consolidating put 32 on 16 cores, at which point the slow ones began failing on
timing rather than on behaviour. `Harness::headless` now caps concurrent
driving threads. See `../BUILD.md` for the measurements and the two rules that
follow from it.

**Still on `set_var`, deliberately.** `library_foundation.rs` sets
`LUMA_CONFIG_DIR` and `LUMA_CACHE_DIR` per test, and `add_tracks_flow.rs` sets
`LUMA_CACHE_DIR`. These never open a harness — they drive `Library` directly on
the test thread, where the environment fallback still reads fresh — and
`library_foundation` serializes itself with its own `environment_lock()`. They
pass, but they are the last process-global writes in the suite and they would
break if either file ever grew a parallel test that raced its neighbour.
`LUMA_CACHE_DIR` is the one knob not yet on `Runtime`; folding it in and
handing `Library` its paths explicitly is the finish line.

### What was planned

48 files in `crates/agent/tests/`, 111 `#[test]` functions, one binary each.
Each binary links `luma-app` + `luma` (all of src-tauri, via dev-dependencies)
+ vendored gpui, and weighs ~120 MB.

### The prerequisites are bug fixes

Per-file *process* isolation is currently load-bearing for exactly one reason:
process-global `std::env` state. Merging files without fixing that is unsound.
But three of these are already unsound *today* — `tab_chrome.rs`,
`dialog_focus.rs` and `empty_panel.rs` each run multiple parallel tests that
race on the same env var, and pass on timing luck. So the prerequisites below
are worth landing on their own merit, consolidation or not.

- **P1 — `LUMA_MOTION_SCALE` is cached in a `OnceLock`**
  (`crates/ui/src/motion.rs:714-724`). Read once per process, so two files
  wanting different scales cannot share a binary. `dialog_morph` and
  `tab_chrome_pixels` want `10`; `add_tracks_pixels` wants `3`. Drop the
  `OnceLock` and read fresh — it caches one env lookup against a 200 ms tween,
  so the cache buys nothing.
- **P2 — `LUMA_MOTION` is sticky.** `support/mod.rs:258-259` sets it to `off`
  only when unset, so the first motion-on test in a binary silently un-snaps
  every later fixture and turns geometry assertions into races against a tween.
  Delete the env path; route through the per-`App` `Fixture::with_motion()`
  that already exists at `support/mod.rs:225-228`, which is the correct form.
- **P3 — `LUMA_CONFIG_DIR` / `LUMA_FIXTURES_ROOT` are set process-wide**
  (`support/mod.rs:251`, `:545`) and read later, on the pump thread, inside
  `Library::open()` (`crates/app/src/library.rs:1752-1758`, uncached). Because
  `pump::run` calls `on_ready` *before* `Backend::open`
  (`crates/agent/src/pump.rs:181-187`), the read happens after `Fixture::open()`
  has already returned to the test thread — so parallel tests interleave and
  last-writer-wins for both apps. Fixed by carrying the paths in the harness
  `Runtime` and deleting both `set_var` calls. This is the change that removes
  the three existing races.

  **The `on_ready` ordering is not the thing to fix, and must be left alone.**
  It looks like the bug and it is only half of it: with the values no longer
  in shared global state, nothing the app reads at startup can be changed by
  anyone else once `run` has been called, whichever order those two lines are
  in. Moving `on_ready` after `Backend::open` was tried and **made the suite
  worse** — three headless runs at 31/32 (`add_tracks_flow` twice, `graph`
  once) against 32/32 either side of the change under heavier load. Handing
  the client over first is load-bearing: it lets the caller build its
  interpreter while the app opens, so the first command lands as the window
  comes up. Deferring it puts the interpreter's construction between the two,
  and fixture delays — which start ticking at app open — burn down before the
  script arrives to watch them.
- **P4 — `LUMA_STAGE_GPU` is sticky** (`crates/agent/src/lib.rs:98-108`, set
  from `config.mode` only when unset). The first harness in a process decides
  for all of them. Make it a `Config` field. Grouping headless and pixel into
  separate binaries (below) also avoids it, but the field is the honest fix.
- **P5 — `Harness` has no `Drop`** and the pump thread is never joined, so
  teardown of the previous app's tokio runtime, SQLite pool and Metal device
  overlaps the next test. **Deliberately not done.** Joining would mean waiting
  on a thread that may be wedged inside gpui — the exact case
  `PumpClient::call` refuses to block on — so a hung app would turn a clean
  timeout into a hung suite. It would also need the interpreter torn down
  first, and the QuickJS context holds its own clones of the client, so there
  is no cheap disconnect. The concurrency cap bounds how much teardown can
  overlap, which is the property that actually mattered; if the accumulation
  ever bites, the fix is a bounded wait on a completion channel, not a join.

Nothing in gpui blocks this. `HeadlessAppContext::with_platform` holds no
statics and no singleton, `MetalHeadlessRenderer::new()` builds a fresh device
per call, and `dialog_morph.rs` already runs five parallel pixel apps in one
process. The one genuine once-per-process call, `current_platform`, is already
guarded by the `TEXT_SYSTEM` `OnceLock` at `crates/agent/src/pixel.rs:37` —
consolidation makes that *safer*, not riskier.

### Grouping

Group by cfg gate, because the gates already separate the incompatible env
regimes, and keep each group's motion scale uniform (until P1 lands).

| target | gate | files |
|---|---|---|
| `pixel_suite_guard` | none — **stays its own target** | 1 |
| `unit` | none, no app | `harness`, `mcp` |
| `headless` | `app` | 22 |
| `chat` | `app` | `agent_chat` — split out on wall-clock coupling, not on gate |
| `app_pixel` | `all(app, pixel)` | 8 |
| `ui_pixel` | `pixel` | 4 |

`pixel_suite_guard.rs` must **not** be merged: it is
deliberately ungated so that a feature-off run has something to say, and
folding it into a gated target reopens the "`ok. 0 passed` in 0.00s looks
green" trap its module doc describes.

Mechanically, each merged target is a thin `tests/<group>/main.rs` — cargo's
directory layout for integration tests — that pulls in the shared support
module with `#[path = "../support/mod.rs"] mod support;` and then names each
member with `mod add_tracks_empty;`. The `main.rs` form matters: a top-level
`tests/<group>.rs` resolves its submodules against `tests/`, not against
`tests/<group>/`. Members reach the shared module through `use super::support;`
rather than declaring their own `mod support;`, which would compile a second
copy — and a second `support::chat::GATE`, quietly undoing the serialization
that module exists for. An `#![cfg(...)]` inner
attribute on a `mod`-included file applies to the module item, so cfg-ing a
group member out removes it and all its tests — exactly today's semantics. Each
file's private `fn harness()`, `fn fixture()` and `const SCRIPT` namespace
cleanly inside its own module.

### Invocation

`--test <file>` becomes `--test <group> <filter>`:

```sh
cargo test -p gpui-agent --test headless tab_chrome    # was: --test tab_chrome
# CARGO_TARGET_DIR must be absolute — cargo resolves it against the cwd, not
# the workspace root. See BUILD.md.
CARGO_TARGET_DIR="$(git rev-parse --show-toplevel)/gpui/target-pixel" \
    cargo test -p gpui-agent --features pixel --test app_pixel
```

### Estimated win

From `BUILD.md`'s measurements, one test target costs ~5.9 s `user` to check
and ~41 s CPU to rebuild-and-link after a touch.

- **`cargo build`/`test --no-run --all-targets`**: 37 links become 5, with 11
  targets still standalone — so 48 becomes 16 today, and ~6 once the held-back
  `visualizer*` / `tracks` / `track_editor_budget` files fold in. The merged
  binaries are fatter and link slower individually, so call it ~3x off the link
  tail now and ~4x after the second step.
- **`cargo check --all-targets`**: `check` does not link, so the win is only
  the 32 fewer crate typechecks, each of which re-typechecked
  `support/mod.rs`. Call it ~2x now, not the 3x the target count suggests.
- **Disk**: ~120 MB per test binary per feature permutation, so 37 of them
  collapsing to 5 is where §1's 377 GB starts coming down.

These are estimates from single-sample CPU measurements under contention.
Re-measure in the quiet window, after §1, before quoting them.

**Test wall time did not change and was never going to.** The suites are bound
by the clock, not the CPU — `headless` runs 32 tests in ~115 s at a
concurrency cap of 6, and ran the same 32 in ~92 s at 16 while failing two of
them. Consolidation buys build and link time and disk. It does not buy a faster
test run, and pushing parallelism to chase one costs correctness.

## Known per-frame cost: route content is rebuilt every frame

Measured 2026-08-23 with the frame-cost probes in
`crates/agent/tests/app_pixel/{venues,add_tracks}_pixels.rs` and
`crates/agent/tests/ui_pixel/dialog_morph.rs` (all `#[ignore]`, run with
`-- --ignored --nocapture`). They read `app.timings()`, which is the **CPU half
of a frame** — scene build only, never the GPU.

`morph::card` calls its content closure once per layer per frame, and a route's
content is rebuilt from scratch on every frame the window draws. Cost is
therefore proportional to what a route is carrying:

| | drawMs p50 |
|---|---|
| shell, no dialog | 0.5 |
| venue dialog, 1 venue | 1.8 |
| venue dialog, 200 venues (no virtualization) | **10.6** |
| add-tracks browser, 4000 tracks (`uniform_list`) | 1.3 |

**A settled dialog does not pay this**, because nothing requests frames when
nothing is animating. It is paid whenever something *does*: a route morph, a
scrim fade — and a hover fade. `motion::hover_fades_active()` requests an
animation frame for as long as any fade is in flight (`app/src/lib.rs`, root
render tail), and `float::menu_row` puts a fade on **every row**. So dragging
the pointer down a long un-virtualized list rebuilds that whole list every
frame, with no morph involved.

Two things follow, neither of them a bug in `card`:

1. **Virtualize, or accept the cost.** `uniform_list` is what makes the
   add-tracks browser flat in library size; `welcome.rs`'s venue list
   deliberately builds every row so the whole list stays in the tab ring, and
   pays 10.6 ms at 200 venues for it. That trade is fine at ten venues and not
   at two hundred. The seam for having both is already there and unused:
   during a flight the outgoing layer is `ContentMode::PaintOnly`, which owns
   no focus and no hitboxes, so it could virtualize while the interactive layer
   does not. **The trap:** `uniform_list` tracks its offset with
   `UniformListScrollHandle`, a different type from the `ScrollHandle` a plain
   column uses, so swapping representations mid-flight without carrying the
   offset across snaps the outgoing layer to the top of the list.

2. **Never derive a row set per frame.** `add_tracks::browser_matches()` used
   to filter-and-clone the whole library on every read, i.e. every frame, per
   layer. `uniform_list` virtualizes what it *renders*; nothing virtualizes
   what it is *handed*. Caching it as `browser_shown`, recomputed only where
   the library or the query changes, took the browser route from 2.17 ms to
   1.25 ms p50 and the route flight from 2.53 ms to 1.71 ms at 4000 tracks
   (three repeats, tight variance). `chat_history.rs`'s `entries`/`shown`/
   `refilter()` is the same pattern and the one to copy.

### What `drawMs` cannot see, and what it turned out to cost

`drawMs` is the scene build and stops at the renderer's door. Two things follow
that cost a day to learn, both worth writing down:

**The harness does not rasterize when you pump frames.** `app.frames()` builds
scenes; only `app.screenshot()` reaches the GPU (`Window::render_to_image`). So
wall-clock timing around a frame pump measures pump overhead and *nothing* of
the renderer — an early reading of "the blurred flight costs +7.6 ms/frame of
wall" was exactly this mistake, and it was noise. To measure anything on the
GPU from this harness, take screenshots.

**The blur is cheap.** `LUMA_FILTER_PROFILE=1` (see
`gpui_apple/src/metal_renderer.rs`, `mod filter_profile`) reads Metal's own
per-command-buffer GPU timestamps and prints one line per rasterized frame.
Through an add-tracks route morph, five runs:

| | GPU ms |
|---|---|
| settled frame (no filtered layer) | 1.50 p50 |
| blurred flight frame (two filtered layers) | 1.90 p50 |
| …of which the content-filter passes | **0.22 p50** |

So a filtered layer costs a couple of tenths of a millisecond, and the frost is
**not** why a morph stutters. Two structural notes: the scratch textures are
1536×1024 each and do **not** grow with the window, because the card is a fixed
680×416 — so this cost does not scale with display size; and under machine
contention every one of these numbers inflates several-fold, so take the
minimum across runs rather than a mean.

**Album art is cheap too, including broken paths.** Seeding 4000 rows with real
PNGs, and again with every eighth row pointing at a file that does not exist,
is indistinguishable from no art at all within a repeat. gpui's asset system
caches the `Result`, so a failed decode is cached rather than retried per
frame. The suspicion was reasonable and wrong.
