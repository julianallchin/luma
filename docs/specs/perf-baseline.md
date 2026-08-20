# perf baseline

The GPUI migration rests on one claim: **webview perf is too bad**. That claim is
unfalsifiable until it has numbers attached. This spec is the procedure for
producing those numbers from the current React/WKWebView build, per screen,
*before* any port, and the acceptance metrics the GPUI app must beat.

Status: tooling implemented, baseline not yet recorded.

---

## 0. What's implemented

| Piece | Path |
| --- | --- |
| Capture module (rAF deltas, jank, input→paint) | `src/shared/lib/perf-baseline.ts` |
| Boot gate (one localStorage read, dynamic import) | `src/main.tsx` |
| Persistence | existing `append_render_telemetry` → `render-telemetry.log` |
| Extract to repo | `harness/perf/extract-baseline.mjs` (`bun run perf:extract`) |
| Recorded baselines | `harness/perf/baseline-*.json` |

Nothing is wired into a production path. With the flag unset, the cost is a
single `localStorage.getItem` at boot; the module lives in its own chunk and is
never fetched.

---

## 1. What is measured, and why those metrics

Three numbers decide whether a UI feels like software or like hardware. Frame
rate under load, tail latency of the worst frames, and how long a pointer press
takes to become pixels. Everything else is diagnosis, not criteria.

**Frame deltas (rAF).** A `requestAnimationFrame` loop runs for the duration of a
segment and histograms consecutive callback gaps at 0.25ms resolution. The loop
is a *passive observer*: rAF callbacks are serviced after the frame's work, so a
delta is the real end-to-end frame interval including layout, paint, and
compositing — which is exactly the thing the webview is accused of doing badly.
Reported as p50 / p95 / p99 / worst, plus effective fps.

- `budgetMs` is the display's frame budget, derived from the p10 delta snapped to
  a real refresh rate (ProMotion machines will read 120Hz → 8.33ms; do **not**
  hardcode 16.7ms).
- `overBudget.x1 / x2 / x4` count frames slower than 1.5× / 2× / 4× budget. `x2`
  is a dropped frame the user can feel; `x4` is a visible hitch.

**Jank (long-task proxy).** Any rAF gap ≥ 50ms. WKWebView does **not** implement
the Long Tasks API — `PerformanceObserver.supportedEntryTypes` will not contain
`longtask`, and the capture records `longTasks.supported: false`. That is
expected, not a bug; the rAF-gap count is the metric of record and the native
long-task counter is opportunistic only (it will populate if you ever run the
same capture in Chrome/Vite for comparison).

**Input→paint.** On the first `pointerdown` / `pointermove` / `wheel` / `keydown`
of each burst (one sample in flight at a time, so a drag doesn't sample every
move), the capture records:

- `toFrameStartMs` — event timestamp → start of the next rAF callback. Lower
  bound on responsiveness: the frame that will handle the input has begun.
- `toPaintMs` — event timestamp → start of the frame *after* that. By then the
  handling frame's pixels are on screen, so this is an upper bound on
  input-to-photon minus display scanout. **`toPaintMs` p95 is the acceptance
  number.**

Event Timing (`entryType: "event"`) is also observed when the engine supports it,
giving handler duration independent of the paint path.

**JS heap** start/end per segment when `performance.memory` exists — context for
whether a screen leaks, not an acceptance metric.

### Why not a Chrome DevTools trace

A trace tells you *where* the time goes; it doesn't produce a stable scalar you
can hold the GPUI build to a year later. Both are useful — profile with the Web
Inspector when a number looks bad, but the number is what gets committed.

---

## 2. Recording procedure

### Setup (do this once, and repeat it identically for the GPUI build)

1. **Release-profile build, not `bun run dev`.** A Vite dev build with React
   StrictMode double-renders and un-minified code is not the product; measuring
   it inflates the baseline and makes the migration look better than it is. Use:

   ```sh
   bun run tauri build --debug     # or a full `bun run tauri build`
   ```

   and launch the produced app bundle. (`bun run tauri dev` is acceptable only if
   every capture — web *and* GPUI — is done the same way, and the JSON is labelled
   as such; `build.mode` in the dump records which it was.)
2. **Plug in.** Battery power throttles the GPU on Apple silicon. Mains, lid open,
   external display disconnected unless you're specifically baselining it.
3. **Quiet machine.** No Xcode/cargo builds, no video calls, no Time Machine. Let
   the app sit idle 60s after launch so the updater check, Python env setup, and
   first sync have finished.
4. **Same window size every time.** Maximize; note that `viewport` is recorded in
   the dump and a GPUI comparison at a different size is not a comparison.
5. **Same data every time.** Record which venue / track / pattern you used in the
   run notes below. A 40-track library and a 4000-track library are different
   products.

### Arm the capture

Open the Web Inspector on the app window (Safari → Develop → *your machine* →
Luma; requires `Develop` menu enabled in Safari and the app built with
devtools). In the console:

```js
localStorage.setItem("luma:perf-baseline", "1")
location.reload()
```

On reload a `PERF` badge appears bottom-right and the console prints
`[perf-baseline] armed.`. Close the inspector — segments are driven by hotkey
(Ctrl+Alt+1..8 start the canonical segment by number below, Ctrl+Alt+0 stops,
Ctrl+Alt+9 dumps; the badge shows `REC <label>` while recording), so the
inspector's own cost never pollutes a segment. `window.__lumaPerf` remains the
console fallback.

### The segments

Each is `__lumaPerf.start("<label>")`, then drive the app by hand for the stated
duration, then `__lumaPerf.stop()`. **Start the interaction first, then start the
capture** — a segment that includes 3 seconds of stillness before you begin
scrolling reports a fake p50. Stop the capture while still interacting.

Use these labels verbatim; the GPUI run must reuse them.

| # | Label | Screen | Drive it like this | Duration |
| --- | --- | --- | --- | --- |
| 1 | `track-list-scroll` | Track browser (`src/features/tracks/components/track-browser.tsx`) | Continuous trackpad flick-scroll top→bottom→top, no pauses. Cover the whole list at least twice. | 20s |
| 2 | `graph-pan-zoom` | Pattern editor, `/#/pattern/:id` — pick the **largest** pattern you have | Continuous: drag-pan across the canvas, pinch/scroll zoom out to fit, zoom back in to a node, repeat. Never let the canvas be still. | 20s |
| 3 | `track-editor-playback` | Track editor, `/#/track/:id` or `/#/venue/:id/edit` | Press play; let the timeline playhead run. Don't touch anything else — this measures the idle-animation cost of the timeline + waveform. | 30s |
| 4 | `track-editor-scrub` | Same screen | Drag the playhead / scrub the timeline continuously, and drag one annotation edge back and forth. | 20s |
| 5 | `visualizer-live` | Perform / stage visualizer with playback running and a busy scene (many fixtures lit, strobe or movement active) | Hands off after starting playback. This is the three.js path — the one the wgpu renderer replaces. | 30s |
| 6 | `agent-pane-streaming` | Track-editor agent chat (`chat-sidebar.tsx`) | Send a prompt that produces a long, tool-heavy response. Capture *during* the stream. Scroll the transcript while it streams for the last 10s. | until the stream ends (≥30s) |

Two more, cheap and worth having:

| # | Label | What | Duration |
| --- | --- | --- | --- |
| 7 | `idle-welcome` | Welcome screen, untouched. The floor: whatever this costs, every other screen pays too. | 15s |
| 8 | `venue-tab-switch` | Repeatedly switch Universe → Edit → Perform → Universe, ~1s on each. Measures route-mount cost, which shows up as `overBudget.x4`, not as p50. | 20s |

### Dump

```js
__lumaPerf.table()   // sanity-check the numbers before committing them
__lumaPerf.dump()
```

`dump()` writes the payload into `render-telemetry.log` (via the existing
`append_render_telemetry` command), copies it to the clipboard, and prints it.
Then, from the repo:

```sh
bun run perf:extract -- --name web-baseline-<yyyy-mm-dd>
```

which writes `harness/perf/web-baseline-<date>.json` and prints the summary
table. Commit that JSON. Add a short run-notes block to the PR: machine, macOS
version, display + refresh rate, library size, which venue/track/pattern, and
whether it was a `--debug` or release build.

### Disarm

```js
localStorage.removeItem("luma:perf-baseline")
```

---

## 3. Acceptance metrics

The GPUI build re-runs **the same eight segments, same labels, same machine, same
data, same window size** and produces `harness/perf/gpui-baseline-<date>.json`.
A screen has migrated successfully when, against the recorded web numbers for
that label:

| Metric | Requirement |
| --- | --- |
| `frames.p50Ms` | ≤ `budgetMs` (i.e. hits the display's native rate) **and** ≤ web p50 |
| `frames.p95Ms` | ≤ `budgetMs × 1.5`, and no worse than web p95 |
| `frames.p99Ms` | ≤ web p99 × 0.5 — halving the tail is the point of the migration |
| `frames.overBudget.x2` | ≤ web `x2` × 0.25, normalized per second |
| `frames.overBudget.x4` | **0** for segments 1–5 and 7. A visible hitch during scroll or playback is a hard fail. |
| `jank.count` | 0 for segments 1–5 and 7. Non-zero is allowed only where a real workload lands on the UI thread (6 and 8). |
| `input.toPaint.p95Ms` | ≤ 2 × `budgetMs`, and ≤ web p95 × 0.5 |
| `frames.fps` | within 5% of `estimatedRefreshHz` for segments 1–5 |

Two carve-outs, stated up front so they aren't argued after the fact:

- **Segment 6 (`agent-pane-streaming`)** is bounded by network and model
  throughput, not rendering. The requirement is only that `p99Ms` and
  `overBudget.x4` improve and that `input.toPaint.p95Ms` meets the general bar —
  streaming must not make the pane unresponsive. Absolute fps is not a criterion.
- **Segment 8 (`venue-tab-switch`)** measures mount cost. `x4 > 0` is expected
  during a mount; the requirement is that the *count* drops by ≥ 4× and the worst
  frame is under 100ms.

### If the web baseline already passes

Then that screen is not evidence for the migration and must not be cited as
such. Record it, say so, and let the migration stand on the screens that fail.
This is the point of writing the criteria down before the numbers exist.

---

## 4. Interpreting a bad number

- p50 above budget with a flat histogram → steady per-frame cost. Layout or
  paint, not GC. Profile with the Web Inspector timeline.
- p50 fine, p99 terrible, `jank.count` high → discrete stalls. GC, a sync
  `invoke`, or a React commit that fans out. Correlate with `jsHeapBytes`.
- `input.toPaint` bad while frames are fine → the input is queued behind
  something, or a commit lands a frame late. This is the class of problem the
  agent pane's content-visibility virtualization and the graph editor's
  transform-only animation already attacked; check whether the win held.
- Everything degrades over the segment → a leak. Re-run at 2× duration and
  compare `jsHeapBytes.start` vs `.end`.
