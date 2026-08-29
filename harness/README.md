# Component screenshot harness

Captures screenshots of the same UI component rendered by both stacks so an
agent can Read the two PNGs side by side and judge visual parity:

- **web** — the real React components from `src/shared/components/ui`,
  rendered on a dedicated Vite page (`/harness.html`) and captured with
  Playwright **WebKit** (closest engine to the WKWebView Tauri uses).
- **gpui** — the same components from `luma-ui`, the GPUI design-system crate
  (`gpui/crates/ui/src`), rendered in a borderless fixed-bounds window and
  captured with `screencapture`. The harness itself
  (`gpui/crates/harness`) owns no component code — it only registers
  fixtures, so what it captures is exactly what the GPUI app renders.

## Fixture contract

A fixture is one component in one deterministic state, identified by an id.
The id must exist in **both** registries rendering the **same state**:

- web: `src/harness/fixtures.tsx`
- gpui: `gpui/crates/harness/src/fixtures.rs`

Both wrap the component in a `#272727` background with 24px padding. When
porting a component, add the gpui fixture with the same id, capture both, and
compare.

## Fonts

Both sides render **Inter** from the same two TTFs in `harness/fonts/`
(rsms/inter v4.1). gpui embeds them with `include_bytes!` +
`text_system().add_fonts()` in `luma_ui::fonts`, which the harness and the
GPUI app both call; the web page
`@font-face`s them via `harness/fonts.css`, linked from `harness.html` and
served off the Vite dev-server root.

This matters because Inter is not a macOS system font: without it both stacks
silently fall back (to different faces), and every typography comparison is
noise. Note the *app* has the same latent problem — `src/App.css` declares
`font-family: Inter, …` but the app ships no `@font-face`, so on a machine
without Inter installed the real UI renders Helvetica.

## Capture

```sh
# web → harness/shots/web/<id>.png  (2x pixels)
node harness/shot-web.mjs --all
node harness/shot-web.mjs button select

# gpui → harness/shots/gpui/<id>.png  (2x pixels on retina)
cd gpui
cargo run -p luma-gpui-harness -- --list
cargo run -p luma-gpui-harness -- --fixture button
```

Then Read `harness/shots/web/<id>.png` and `harness/shots/gpui/<id>.png` and
compare: geometry (heights, padding, borders), exact ladder colors, typography.

## Gotchas

- `screencapture` needs the invoking terminal to have Screen Recording
  permission (System Settings → Privacy). First run may silently capture the
  wallpaper instead of the window — if the PNG has no component in it, that's
  why.
- The gpui capture grabs a screen region, so the window must be unobstructed
  at (200, 200) for ~1s. Don't run captures while dragging windows around.
- Known v1 gap on the gpui side: no letter-spacing (`tracking-wider`) support.
  Call it out in comparisons rather than failing on them. On `button` it costs
  ~10 device px of ink width (13 glyphs × 0.05em × 9px × 2x). On self-sizing
  controls (`selector`, `dropdown-closed`) it also shrinks the *box*, since the
  width is derived from the widest option's ink.
- Sizes in `fixtures.rs` are window points (content + 24px padding); the web
  shot hugs content, so minor canvas-size differences between the two PNGs are
  expected — compare the component, not the canvas.

---

# Renderer goldens

`harness/goldens/scenes/` is the frozen record of what the **three.js**
renderer draws, captured before the wgpu port touches it
(`docs/specs/wgpu-renderer.md` §7). Eight scenes × three clock values = 24
frames at 1600×1000, plus a `manifest.json` carrying the complete input
description of every frame (camera pose, render settings, fixture poses and
definitions, primitive state, sha256). A golden is re-derivable from the
manifest alone — which matters, because once the port lands and three.js is
deleted these PNGs are the *only* copy of the reference look. They are
committed on purpose.

> **Stale on purpose, for two scenes.** `led-bar` and `venue-no-haze` were
> captured while a procedural LED bar fired out of its housing's `+depth` face.
> A fixture now rests along its **mount normal** (`docs/design/venue-graph.md`,
> phase 0), so those two scenes changed — `scenes-wgpu/` was recaptured and
> `src/features/visualizer/components/procedural-fixture.tsx` was turned to
> match, but re-shooting the three.js reference needs a running app. Compare
> those two frames against `scenes-wgpu/`, not against these.

The scenes live in `src/harness/golden-scenes.ts` — plain data, no database,
no eval engine, no IPC. `src/harness/scenes-3d.tsx` mounts the *real*
`<StageVisualizer>` against them on `/harness-3d.html?scene=<id>`.

```sh
node harness/shot-visualizer.mjs --all                 # -> harness/goldens/scenes/
node harness/shot-visualizer.mjs single-mover strobe-duty
node harness/shot-visualizer.mjs --all --out /tmp/run-b # a second run to diff

# stability gate: two runs must be SSIM >= 0.999 frame for frame
node harness/compare-shots.mjs harness/goldens /tmp/run-b
# port comparison: sigma=4 pre-blur kills haze grain, keeps structure (§7.3)
node harness/compare-shots.mjs harness/goldens harness/wgpu --blur 4 --min 0.98
```

## What makes a frame reproducible

Four pins, and all four are load-bearing:

1. **No backend.** Stores are seeded directly from the scene; fixture state
   arrives through `PrimitiveOverrideContext` — the injection seam the app
   already has for pattern preview. `src/harness/tauri-stub.ts` no-ops the IPC
   bridge, and the manifest records every command the page attempted — only
   the universe-state `listen` and render telemetry, none of which returns
   data the frame depends on.
2. **`frameloop="never"`.** The capture drives frames with r3f's `advance(t)`,
   which in that mode sets `clock.elapsedTime` to exactly `t`. That clock is
   what haze noise drift and strobe phase read.
3. **One page load per frame.** `uFrame` (the jittered ray-start walk) and the
   temporal history both have to start from zero, or a frame's grain depends on
   which frames were captured before it.
4. **64 warm-up advances at the same `t`.** The temporal EMA needs to converge;
   repeated advances at one `t` leave delta at zero, so only the accumulator
   moves.

With those in place the capture is bit-identical run to run, not merely close:
two full runs produce 24/24 byte-equal PNGs (SSIM 1.000000, mean |Δluma| 0).
Any drift shows up in `compare-shots.mjs` at `--blur 0`.

## Gotchas

- The page hides the visualizer's DOM overlays (corner ticks, fps readout,
  editor toolbars) via a `z-10` rule in `harness-3d.html`. Playwright clips the
  *page* to the canvas box, so without it app chrome lands in every golden.
- `--browser chromium` works and is faster, but WebKit is the engine that
  matches the WKWebView the app actually ships in. Goldens are WebKit.
- Readiness is "no fallback cube left under any fixture group, and every stage
  piece registered". The capture then settles two rAFs so the post-mount
  scaling/light-registration effects land — without that, the 121-fixture scene
  intermittently captures a fixture mid-setup.

---

# Perf baseline

`harness/perf/` holds the recorded webview perf baselines the GPUI port has to
beat. The capture lives in the app (`src/shared/lib/perf-baseline.ts`, armed by
`localStorage["luma:perf-baseline"] = "1"`), dumps ride the existing
render-telemetry log, and

```sh
bun run perf:extract -- --name web-baseline-<date>
```

pulls the newest dump into `harness/perf/<name>.json`. Full procedure and the
acceptance metrics: `docs/specs/perf-baseline.md`. Recording is a hand-driven
session — don't fake it with a script.

---

# Graph-editor gauntlet

`harness/gauntlet/` is the visual quality bar for the GPUI pattern-graph
editor: pixel-true captures of the **real** web graph canvas plus the exact
style values behind them.

```sh
bun harness/gauntlet/extract-fixtures.ts          # graphs -> fixtures/<pattern>.json
cargo run --manifest-path src-tauri/Cargo.toml \
  --bin dump_node_types > harness/gauntlet/node-types.json
node harness/gauntlet/shot-graph.mjs --all        # -> web-<pattern>-<view>.png (2x)
node harness/gauntlet/shot-graph.mjs --all --out /tmp/run-b   # stability diff
```

- `fixtures/<pattern>.json` — the graph (real saved implementation, positions
  normalized through the app's own `layoutGraph()`), the deterministic
  view-node signals, and the closeup viewport. **The GPUI side must render
  these same files**; that is what makes the two captures comparable.
- `node-types.json` — the compiled node catalogue (`get_node_types()`), needed
  because ports and param defs are not stored in a saved graph.
- `style-spec.md` — every colour, radius, border, font, port-ring dimension and
  wire parameter with its `file:line`, measured off the running page.

`src/harness/graph-canvas.tsx` mounts the *real* `<ReactFlowEditor>` and the
real node components at `/harness-graph.html?pattern=<id>&view=<whole|closeup>`.
Two full runs produce 4/4 byte-identical PNGs.
