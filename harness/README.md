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
- Known v1 gaps on the gpui side: no letter-spacing (`tracking-wider`) support,
  and Inter falls back to the system font if not installed. Call these out in
  comparisons rather than failing on them.
- Sizes in `fixtures.rs` are window points (content + 24px padding); the web
  shot hugs content, so minor canvas-size differences between the two PNGs are
  expected — compare the component, not the canvas.
