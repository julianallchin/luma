# Component screenshot harness

Captures screenshots of the same UI component rendered by both stacks so an
agent can Read the two PNGs side by side and judge visual parity:

- **web** — the real React components from `src/shared/components/ui`,
  rendered on a dedicated Vite page (`/harness.html`) and captured with
  Playwright **WebKit** (closest engine to the WKWebView Tauri uses).
- **gpui** — GPUI ports of the same components (`harness/gpui/src/fixtures.rs`),
  rendered in a borderless fixed-bounds window and captured with
  `screencapture`.

## Fixture contract

A fixture is one component in one deterministic state, identified by an id.
The id must exist in **both** registries rendering the **same state**:

- web: `src/harness/fixtures.tsx`
- gpui: `harness/gpui/src/fixtures.rs`

Both wrap the component in a `#272727` background with 24px padding. When
porting a component, add the gpui fixture with the same id, capture both, and
compare.

## Fonts

Both sides render **Inter** from the same two TTFs in `harness/fonts/`
(rsms/inter v4.1). gpui embeds them with `include_bytes!` +
`text_system().add_fonts()` in `harness/gpui/src/main.rs`; the web page
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
cd harness/gpui
cargo run -- --list
cargo run -- --fixture button
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
