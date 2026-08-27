> Extracted from a clone of the zeronsh/comet ("zeron") gpui codebase on 2026-08-22.

# comet (zeron) UI spec — extracted for luma

Repo: `/private/tmp/claude-501/-Users-julian-github-luma/9e4cb350-b19e-4d37-a75c-eb7fd33fb805/scratchpad/comet`. Paths below are relative to `crates/ui/src/`. `apps/zeron/src/` is a clap CLI only — **all gpui code lives in `crates/ui`**, and the window opens from `lib.rs:197`, not `main.rs`.

## 0. Theme foundation (everything below cites these)

Two appearances, dark canonical. `theme.rs:625-700`.

**Dark palette (resolved):** `bg grey(6)=#060606` · `surface grey(13)=#0d0d0d` · `surface_raised neutral(0.235)` · `surface_card grey(0x0e)` · `surface_dialog grey(0x10)` · `surface_overlay grey(0x16)` · `element_hover hsla(0,0,0.92,0.11)` · `element_active hsla(0,0,0.92,0.16)` · `border hsla(0,0,1,0.08)` · `border_strong hsla(0,0,1,0.14)` · `text neutral(0.922)` · `text_muted neutral(0.708)` · `text_faint neutral(0.556)` · `accent oklch(0.673,0.182,276.935)` (indigo-400) · `danger oklch(0.704,0.191,22.216)` · `warning oklch(0.828,0.189,84.429)` · `success oklch(0.765,0.177,163.223)` · `input_bg hsla(0,0,1,0.03)` · `selection hsla(0.66,0.6,0.55,0.35)` · `caret hsla(0.66,0.7,0.7,1)` · `code_text oklch(0.811,0.111,293.571)` (violet-300) · `code_wash violet-400 @ 12%`.

**Fonts:** `font_sans "Geist"`, `font_mono "Geist Mono"`, fallbacks Helvetica/Menlo on macOS (`theme.rs:414-421`, `965-985`).

**The three alpha families** — the load-bearing idea. Every call site quotes a **dark-mode alpha**; the light value is derived, so one number keeps both appearances in the relationship the dark tuning established (`theme.rs:810-865`):

- `ink(a)` — translucent **fill** (chip plates, hover washes): dark `hsla(0,0,1,a)`, light `hsla(0,0,0,a × INK_FILL_SCALE)`, `INK_FILL_SCALE = 1.0` (`theme.rs:138`).
- `hairline(a)` — translucent **edge** (borders, dividers, rings): dark `hsla(0,0,1,a)`, light `hsla(0,0,0, min(a × 1.35, 0.5))` — `INK_HAIRLINE_SCALE = 1.35` (`theme.rs:144`). Separate from `ink` because "a 1px line needs *more* ink on white, a plate needs less."
- `wash(a)` — softened ink stopping short of pure white/black so hover plates read as tinted glass: dark `hsla(0,0,0.92,a)`, light `hsla(0,0,0.10,a)`.
- `scrim(alpha_dark)` — always black in both modes, light scaled to ~53%: `SCRIM_ALPHA_DARK = 0.60`, light `0.32 × (a/0.6)` (`theme.rs:869-885`).
- `band()` — recessed strip behind palette headers/footers: dark `hsla(0,0,0,0.16)`, light `0.045` (`theme.rs:893-900`).

**Selection language** (`theme.rs:903-985`) — one recipe, worth copying wholesale: selection and hover share the **same fill**; only an **inset ring** distinguishes selection. `card_selected_bg()` = `wash(0.11)` dark / `wash(0.06)` light. `card_selected_shadows()` = a single `BoxShadow { blur 0, spread 1px, inset: true, color: hairline(0.09) }`. The comment at `theme.rs:940-960` records three rejected drop-shadow recipes: a drop shadow is a filled rect painted *behind* the element, so behind a translucent fill it shows through as an opaque grey plate. **Nothing may paint behind a glass chip.** `user_bubble_bg()` = `wash(0.08)` — one step softer than selection.

**Spacing/radius constants** (`theme.rs:449-478`): `TITLEBAR_HEIGHT 38` · `TITLEBAR_TOP_PAD 2` · `HEADER_HEIGHT 44` · `STATUS_STRIP_HEIGHT 24` · `TRANSCRIPT_FADE_BAND 24` · `BUBBLE_RADIUS 16` · `PANEL_RADIUS 10` · `CONTROL_RADIUS 6` · `SPACE_XS/SM/MD/LG = 4/8/12/16` · `TEXT_STACK_GAP 1`.

---

## 1. Project picker / file picker dialogs

Two distinct picker families, and the distinction matters for porting.

### 1a. Anchored popovers (`pickers.rs`) — not modals

Six variants (`Branch, Checkout, HarnessModel, Traits, Space, Device` — `pickers.rs:446`) all drive **one** `Popup<PickerKind>` on **one** entity. **No scrim.** They mount via `gpui::deferred(...).priority(1)` + `.occlude()`, anchored to a trigger chip with `snap_to_window_with_margin(px(8))` and a 6px trigger gap (`popover.rs:395-581`).

Card (`popover.rs:307-324`) — the canonical floating surface:
```rust
div().border_1().border_color(hairline(0.10))
     .rounded(px(CARD_RADIUS))   // 12.0
     .shadow_lg().p(px(4.0)).overflow_hidden()
     .text_size(px(13.0)).text_color(theme.text)
     .bg(if theme.is_frost() { theme.glass_overlay() } else { theme.surface_overlay })
```
Frame adds `max_h(px(640))`, `.track_focus()`, `.on_key_down()`, `.on_mouse_down_out(close)` (`pickers.rs:2556-2571`). Widths: Space 280, Device 224, Branch 320, Checkout 224, HarnessModel 360 (flush, `p(0)`), Traits 240.

Menu row (`popover.rs:656-689`): `gap(10) px(8) py(6) rounded(8) text_size(13)`. Lists are `flex_col gap(2) max_h(224) overflow_y_scroll`.

**No virtualization anywhere** — no `uniform_list`, no `gpui::list`. Plain flex columns of all rows in a scroll container; row heights intrinsic. Only cap is `MAX_REF_ROWS = 300` on git refs with a "Showing X of Y" footer. The model list is the only one with a `ScrollHandle` (for `scroll_to_item(active)` on keyboard nav) plus a custom floating scrollbar (track inset 4, thumb 3px → 5px hovered, min 24px, `rounded(w/2)`, `text_faint.opacity(0.5/0.68)`, visible only while hovered — `pickers.rs:2636-2822`).

Keyboard: **no gpui actions/keymap** — a raw `on_key_down` on the frame plus `popover::classify_key` (`popover.rs:275-290`) mapping `up/down`, `ctrl-n/ctrl-p`, `enter`, `cmd-enter`, `escape`, `backspace`. `menu_step` wraps at both ends. The frame carries `.track_focus()` so arrows reach it even while a child input holds focus; searchable pickers focus the input and set a per-kind placeholder. ⌘1–⌘9 jump to model rows, advertised inline via `kbd_hint` chips.

Loading = `skeleton_rows` (28px `rounded(6)` bars, `bg ink(0.04)`, `opacity(0.35 + 0.4×pulse_wave)`, 0.08 stagger, 2400ms period). Errors = `error_row` + Retry button. Empties are one `p(8) 12px text_faint` line, except the no-agents state which takes the whole card (20px icon + 13px title + 12px centered muted body).

### 1b. The real modal file picker — `shell/spaces.rs:1447-2196` (add-space palette, ⌘K)

This is the one to port. `popover::modal_glass("add-space-dialog", viewport, card, 14.0)` → scrim `0.35` (not the standard `0.6` — the standard dim "buried the backdrop hue under the blur and the palette came out a flat grey slab next to the hue-inheriting menus"), card wrapped in `frosted(14.0, MENU_BLUR)`, mounted `deferred(...).priority(2)`, entrance `motion::dialog_in`.

Card: `w(px(680)) rounded(px(14)) border_1 hairline(0.10) bg(glass_overlay()) shadow_lg overflow_hidden flex_col`, `.track_focus()` + `.on_key_down()` + `.on_mouse_down_out(dismiss)` (`spaces.rs:2152-2188`).

Three horizontal bands, and the **header/footer sit a shade deeper than the body** (`popover::band()`), framing the list which stays on the brighter tint:

1. **Search bar** `h(46) rounded_t(14) pl(12) pr(10) gap(10) bg(band) border_b_1`. Contents: a `⌘K` key-cap chip · the input at `text_size(14)` · a `⌘ Enter` primary submit chip · an `esc` chip. Key chip recipe (`spaces.rs:1546-1560`): `h(22) px(6) rounded(5) gap(2) bg(ink(0.05)) text_size(11) font_mono text_muted.opacity(0.7)`. The submit chip is `btn_primary` overridden to `h(22) px(8) rounded(5)` — note the comment: *btn_primary's rounded-8 at this size read as a different component*.
2. **Body** `h(px(330))` — **fixed height on purpose**: "sparse folders, loading skeletons, and device switches must not resize the card." Folder column (breadcrumbs + list) beside a `w(196)` devices/locations rail with `border_l_1`.
   - Breadcrumbs: `px(13) pt(10) pb(2) text_size(11) font_mono`, segments `px(3) rounded(4)`, current `text.opacity(0.85)`, ancestors `text_muted.opacity(0.55)` + `hover → text`, `/` separators at `text_faint.opacity(0.7)`.
   - Folder list: the 6px gutters live on a **wrapper outside the scroll viewport** — "in-content padding can't do it: the wheel's max offset eats bottom padding, and `scroll_to_item` pins the row's bottom to the viewport edge regardless." Inner `overflow_y_scroll track_scroll px(8) gap(2)`. Rows are `menu_row_nav` + `card_selected_shadows()` when active, 15px folder icon at `text_muted.opacity(0.8)`, truncating label, quiet 13px `GIT_BRANCH` glyph on repo rows.
   - Rail rows: `h(28) px(8) rounded(8) gap(8) text_size(12.5)`, active = `card_selected_bg()` + `card_selected_shadows()`, inactive `text_muted.opacity(0.7)` + `hover(element_hover)`. Presence dot `size(5) rounded_full` with an emerald `blur 6px` glow when online.
3. **Footer** `rounded_b(14) bg(band) border_t_1 px(12) py(8) gap(12)` — a `key_hint` legend (↑↓ Navigate · ← Up · → Open · tab Complete).

Keyboard (`spaces.rs:1382-1443`) — the interesting design call: **←/→ act on the folders, not the text cursor**, because "the palette is a navigator first; queries are short and edited with ⌫". `→`/`Enter` open the highlighted folder, `←` goes up, `⌫` on an empty query goes up, `Tab` accepts the completion, `⌘⏎` submits — and the chord acts on the folder **open in the breadcrumbs**, not the highlight, since the highlight auto-rests on row 0 and would otherwise add arbitrary subfolders. Arrows call `list_scroll.scroll_to_item(active)`.

---

## 2. Dialog open/close animation

The whole system is one small module, `motion.rs`, and it is the single most portable thing in the repo.

**Mechanism.** A hand-rolled `CubicBezier` with Newton + bisection solving (`motion.rs:100-208`), exposed as `MotionSpec { duration_ms, delay_ms, curve }`. gpui's `Animation` has no delay, so the delay is folded into the timeline: the animation runs `delay + duration` and `progress()` holds 0 until the delay elapses. `spec.animation()` = `Animation::new(total × speed_scale()).with_easing(|d| spec.progress(d))`.

One sharp gotcha (`motion.rs:196-201`): f32 rounding pushes `sample_y` a hair past 1.0 (observed `1.000000119`), and **gpui's animation element asserts `delta ∈ [0,1]` and aborts** — so `eval` clamps hard.

**Curves:** `EASE_OUT_EXPO (0.16,1,0.3,1)` · `EASE_OUT (0,0,0.58,1)` · `EASE (0.25,0.1,0.25,1)` · `EASE_IN_OUT (0.42,0,0.58,1)` · `EASE_TAILWIND (0.4,0,0.2,1)` · `EASE_RESORT (0.22,1,0.36,1)`.

**Catalog** (`motion.rs:283-317`): `FADE_IN 500 EASE_OUT_EXPO` · `FADE_QUICK 150 EASE` · `MENU_IN 140 EASE` · `MENU_OUT 100 EASE` · `DIALOG_IN 180 EASE` · `SPLASH_OUT 500 EASE +150 delay` · `RESIZE 200 EASE_OUT` · `TAB_SLIDE 150 EASE_OUT` · `COLLAPSE 180 EASE_OUT` · `CHEVRON 200 EASE` · `SCROLL_GLIDE 500 EASE_IN_OUT` · `HOVER_FADE 150 EASE_TAILWIND` · `ZERON_PULSE 2400` · `GRADIENT_SPIN 750`.

**What actually animates.** gpui divs have **no scale or rotation transform** at comet's pinned rev, so every "scale 0.96 → 1" from the web original is approximated as fade + a few px of translate:

- `menu_in` — `opacity(0.3 + 0.7t)` + `top(px(-2 × (1-t)))`, 140ms.
- `menu_out` — `opacity(1-t)` + `top(px(-2t))`, 100ms. Exits deliberately shorter than entrances (Radix convention).
- `dialog_in` — `opacity(t)` + `top(px(2 × (1-t)))`, 180ms.
- `fade_in` — `opacity(t)` + `top(px(4 × (1-t)))`, 500ms expo-out.
- `splash_out` — `opacity(1-t)` + `top(px(-6t))` after a 150ms hold.

**Dismissal is the non-obvious part.** gpui unmounts an element the frame its state drops, so an exit animation needs the state held alive. `Popup<T>` (`popover.rs:66-180`) is a three-phase lifecycle: `open` → `begin_close()` (stamps a closing `Instant`; render keeps mounting with the out-animation and an `.absolute().inset_0().occlude()` overlay so the dying menu eats no clicks) → `reap_popup` spawns a timer for `MENU_OUT.total() × speed_scale() + 20ms` then `finish_close()`. Two accessors enforce the split: `is_open()`/`as_open()` return false/None during the exit so handlers fall through, while `get()`/`closing_since()` keep rendering.

Critically, **`menu_out` takes `t` from the caller**, not from the animation's own delta: `with_animation`'s element-id-keyed clock replays from 0 on remount, and a replay mid-exit is a full-opacity flash. The animation wrapper is used only to pump frames. Exit progress is computed off wall-clock (`popover.rs:350-361`), and the backdrop blur rides it: `blur = MENU_BLUR × (1 - exit)`.

Matching trigger-toggle fix (`popover.rs:145-168`): the card's `on_mouse_down_out` fires on the same press as the trigger's click, so by mouse-up the popup already reads closed and a plain toggle closes-then-reopens. `note_trigger_press_matching()` records at mouse-down whether this popup was mounted; `take_press_was_open()` consumes it at click.

**Hover fades.** gpui `.hover()` styles snap by construction. For CSS `transition-colors` parity, `motion::hover_blend(key, from, to)` + `hover_listener(key)` keep a **global keyed** blend state (150ms `EASE_TAILWIND`) — global rather than element-local precisely because element-id-keyed animations replay on remount, and in a virtualized list every scroll-back-into-view is a remount.

`speed_scale()` (`motion.rs:620`) multiplies every span — a measurement knob. `reduced_motion(cx)` snaps everything.

---

## 3. Sidebar blur

**There is no `background_blur` call anywhere in the repo.** Zero hits. Two separate mechanisms:

**Window-level compositor vibrancy** — this is the sidebar's blur. `lib.rs:248` sets `window_background: Theme::window_background_appearance()`, returning `gpui::WindowBackgroundAppearance::Blurred` when `is_glass()` else `Opaque` (`theme.rs:603-609`). `is_glass()` ≡ `glass().a < 1.0`, i.e. macOS-only (`GLASS_ALPHA = 0.80` on macOS, `1.0` elsewhere).

**The gotcha to carry over:** the value must be **re-pushed after every theme swap**. gpui's macOS backend rips the `NSVisualEffectView` out of the hierarchy whenever the value is anything but `Blurred`, and nothing restores it — hence `appearance::reapply_window_background(cx)` at the end of `appearance::apply` and again at `lib.rs:264`. Zed runs the same loop.

**The sidebar itself paints no blur and no background.** The blur is the whole window's; the sidebar reads through it. Layer stack: blurred desktop → shell root `.bg(theme.glass())` = `grey(8).opacity(0.80)` dark / `grey(0xfa).opacity(0.80)` light → an absolute tone column `bg(wash(0.05))` + `border_r_1 border_color(theme.border)` (`shell.rs:7201-7209`) → the sidebar content column with **no bg at all** (explicitly, `shell.rs:3414-3417`).

The tone column spans the **full window height, under the titlebar to the bottom edge**, and its width rides the same collapse tween so it melts with the sidebar.

**Scene-level backdrop blur** (`frost.rs`) is a different thing — floating cards only. `Frosted::paint` opens `window.paint_layer` and calls `paint_backdrop_blur(bounds, Corners::all(radius), blur)` before painting the child. Gates on `is_frost()` = `cfg!(any(macos, linux))` — scene-level, because it blurs in-app content (Metal / vendored wgpu), not the desktop, so it needs no compositor vibrancy.

`MENU_BLUR = 44.0`. Call sites: popover cards 12/44 · rail preview 12/44 · badge popover 12/44 · change-request card 6/44 · jump pill 15/16 · **composer pill 26/16**. The blur mask radius must equal the element's own `rounded()`.

Two hard-won notes (`frost.rs:1-9`, `:116-120`):
- The single-`paint_layer` wrapper is the whole point. With per-primitive bounds-tree ordering, a hover repaint elsewhere could reassign the card's quads *below* the blur, so washes, dividers and borders got snapshotted and blurred away. Inside one layer the order is structural: blur → shadow → tint → border → rows → text.
- `layered(child)` exists because *inside* a frosted card, equal draw orders render grouped by primitive kind (quads → icons → images), so a close button's circle painted "after" a thumbnail still landed under the image.

**Sidebar geometry:** `SIDEBAR_MIN 208 / DEFAULT 256 / MAX 400`, collapsed = `0.0`, tweened by `RESIZE` (200ms). The outer `pane_container` is `overflow_hidden` with the tweened width while the inner keeps a fixed width, so content doesn't reflow during collapse. `pt(TITLEBAR_HEIGHT=38)` (the titlebar overlays it). Rows `rounded(8) px(8) py(6) gap(2)`, list gap 2, section padding `px(8)`. The resize handle floats on a **zero-width seam** (`w(px(0))` + `left(px(-6))`) with a 12px hit area.

**Edge fades** (`edge_fade.rs`) are the companion piece and genuinely novel: over a see-through blurred backdrop **no painted overlay can fade content out** — "what is behind the window" is not a paintable color. So `edge_faded(band, top, bottom, child)` wraps the subtree in a `gpui::EdgeFade` scope and primitives fade by distance to the wrapper's edges at **per-glyph granularity**. No gradient stops, no colors — an alpha ramp on the primitives themselves. Sidebar uses band 32 with `.fade_overflow_y(&scroll)`; the transcript uses band 24 with `.inset_top(38)` so text vanishes before it can overlap opaque titlebar text. Overflow gating happens at **paint** time, because render-time gating rode the last frame's offset and left phantom fades after a content shrink.

**Window setup** (`lib.rs:197-265`): bounds `1320×880` centered, `window_min_size 900×600`, `TitlebarOptions { title: None, appears_transparent: true, traffic_light_position: point(14,14) }`, `app_owns_titlebar_drag: true`, `window_decorations: Client` on Linux only, `app_id "zeron"`. The titlebar is `h(38)` with **no fill** — an absolute overlay over a full-height content row.

---

## 4. Corner-radius language

The rule, stated plainly by the evidence: **panes are square, floats are round.** The sidebar column, the sidebar tone, the right pane, and the titlebar are all fully square — they meet window edges and each other with sharp corners, separated by a 1px `theme.border` hairline. Rounding is reserved for floating cards, rows, chips, and buttons.

| Radius | Used for |
|---|---|
| 1–3 | rail tick bar (1), rail selected indicator (3), update progress bar (2) |
| 4–5 | ref badges, SHA cells, tab icon slots, breadcrumb segments (4); tooltip cards, key caps, archive pills, chip icon tiles (5) |
| 4.5 | inline code wash (`INLINE_CODE_RADIUS`) |
| 6 | `Theme::CONTROL_RADIUS` — buttons, window controls (24px), header icon buttons (28px), tab chips, notice strips, code-block copy button (5) |
| 7 | "Load more" button |
| 8 | **the workhorse row/chip radius** — session rows, settings nav rows, menu rows, user-menu trigger, picker trigger chips, rail tabs (36px), org rows, model rows |
| 9 | tool chip cards |
| 10 | `PANEL_RADIUS`; code blocks, surface-picker rows, error/input chips, settings option cards |
| 12 | `popover::CARD_RADIUS` — every popover/menu card; also login/restart cards |
| 14 | the add-space modal palette |
| 16 | `BUBBLE_RADIUS` — user message bubbles; `dialog_card` |
| 26 | the composer pill |
| `rounded_full` | avatars (28), status dots (5–6), send/stop button (28), attach button (28), jump pill (30), graph commit nodes, list bullets (5) |

**Mixed corners — exactly three in the whole repo**, each with a clear rationale:

1. `settings/appearance.rs:45-67` — theme-preview miniatures. An `enum Corners { All, Left, Right }` where `Left => rounded_tl(r).rounded_bl(r)` at `r = 10`. The split card's two halves round only their **outer** side so they meet flush down the middle.
2. `pickers.rs:3573-3582` — the model picker's rail indicator: `w(3) h(20) top(8) right(-4)` with `rounded_tl(3).rounded_bl(3)`. A **left half-capsule**; the flat right edge presses against the rail/pane border it hugs.
3. `markdown/render.rs:231-236` — blockquote: `border_l_2()` accent rail with `rounded_tr(6).rounded_br(6)`. Sharp on the left where the rail is, rounded on the free right side.

Same principle all three times: **round the free edges, leave the meeting edge sharp.** Same family, though not `rounded_*` mixing: the add-space palette's `rounded_t(14)` header and `rounded_b(14)` footer against a square-cornered body.

---

## 5. Chat thread + composer

### 5a. Transcript (`transcript.rs`, 6899 lines)

**Virtualization: `gpui::list()` + `ListState`**, not `uniform_list`, not a plain column. `ListState::new(0, ListAlignment::Bottom, px(320.0))` — `OVERDRAW_PX = 320`. Alignment is `Bottom` for the main transcript, `Top` only for subagent-doc instances; `transcript.rs:1936-1948` explains why: a Top list materializes past-end offsets to a concrete position every frame, so a parked spring can't re-glue — only `Bottom` re-glues to the `None` sentinel.

**One row per *block*, not per message** (`transcript.rs:4-15`). A user message is one bubble row; an assistant text part becomes one row per top-level markdown block (`{msgId}#{partId}.{blockIx}`); consecutive tool calls fold into one group row. This makes streaming flat-cost: only the tail rows' content hashes change per commit, so `diff_rows` splices O(changed rows) and the settled prefix keeps its render caches. Row versions are content hashes (FNV-1a over block bytes, low bit = streaming).

Three caching tiers: a row-set cache keyed by entry fingerprint; a parse cache (`IncrementalParser` per streaming row, `Arc<BlockTree>` for completed, with a `Handoff` on the live→complete flip); and a render cache of flattened text+runs keyed `(row_key, top_ix, elem_ix)`, invalidated wholesale on `theme_generation()` change.

One splice subtlety worth stealing: when the diff's old and new counts are **equal**, it calls `list.remeasure_items(range)` rather than `list.splice(...)`, because splice resets items to hint-less Unmeasured and clobbers the scroll anchor — that was the end-of-turn jump.

**Stick-to-bottom is a velocity spring**, not a snap: damping 0.7, stiffness 0.05, mass 1.25, 60fps sub-stepping capped at 8 catch-up frames, plus an EMA of target growth (`0.12`) fed forward as `target_vel` so it tracks a *growing* target, with the chase point leading by `min(target_vel×9, 32)` px. Pin breaks when distance grows >2px; re-sticks when `distance <= 70 && distance < previous` — direction-aware, so a small wheel-up notch inside the band doesn't snap back. Jump-to-bottom threshold 320px.

There's also an **own-turn anchor**: on send, the prompt parks at the top (`inset = TITLEBAR_HEIGHT + 10 = 48`) and the transcript reserves `usable − turn_height` as trailing pad on the last row, consumed 1:1 as the reply grows so the held layout never moves. Entry glide is `err × (1 − 0.85^frames)`.

**Row layout.** Shared frame (`transcript.rs:3994-4037`): `w_full flex justify_center pt(top_gap) pb(bottom_pad) px(px(48))`, inner column `max_w(px(736)) min_w_0`.

- **User message** — right-aligned bubble, `max_w(736 × 0.8 = 588.8)`, `bg(user_bubble_bg())` = `wash(0.08)`, `rounded(16) px(16) py(10) text_size(14) line_height(22)`, `opacity(0.65)` while the optimistic echo is pending. `min_w_0` is load-bearing: without it gpui's unwrapped min-content width prevents shrink and long prompts clip off the left edge.
- **Assistant** — **no container at all.** No bubble, no background, no border, no left rail, no avatar. The block element goes straight into the frame, left-aligned, full 736 column.
- **There are no role labels or avatars anywhere.** Role is conveyed purely by alignment plus the presence/absence of a bubble plate. That's the whole visual grammar.
- **Error chip**: `min_h(34) rounded(10) border_1 danger.opacity(0.16) bg danger.opacity(0.05) px(8) py(7) text_size(12)`, 20px `rounded(6)` icon tile, message **wraps** (deliberately not truncated).
- **Input chip** (agent asked a question): `h(34) rounded(10) border_1 hairline(0.08) bg ink(0.045)`, value truncates. Resolution never recolors.

**Spacing rhythm** — `top_gap_for(prev, row)` (`transcript.rs:1138-1155`), in priority order: turn start → 16 · two markdown rows split from the same text part → **12, deliberately identical to `MD_BLOCK_GAP`** so the live→split handoff can't shift a pixel · either side is a tool group → 12 · otherwise 8. First row top pad = `38 + 16 + 10 = 64`. Last row bottom pad = measured composer clearance + 24 fade band + 8 + runway.

**Markdown** (`markdown/render.rs`) — pulldown-cmark with `TABLES | STRIKETHROUGH | TASKLISTS`. `MD_BLOCK_GAP 12` · body `14/22` · code `12.5/18`, `CODE_PADDING 12/10`.

- Headings: h1 `19/27`, h2 `16/24`, h3 `15/22`, h4+ `14/22`, weight SEMIBOLD, **no heading margins and no rules** — spacing comes entirely from the row gap.
- Lists: `flex_col gap(4)`, item `flex_row gap(8)`, marker `min_w(18)` in `accent.opacity(0.85)`; the unordered bullet is a **real 5px `rounded_full` disc**, not a "•" glyph — the glyph was rejected as too small at 14px.
- Inline code: `font_mono`, `code_text`, and the wash is painted as **rounded quads by a canvas underlay** (radius 4.5, pad_x 2, inset_y 2) because `TextRun::background_color` can only be a square box. Adjacent code runs merge into one wash box.
- Code blocks: `rounded(10) bg(ink(0.035)) border_1 theme.border overflow_hidden`, optional language header `px(12) py(5) border_b_1 bg(ink(0.02)) text_size(11)`, body `overflow_x_scroll px(12) py(10) whitespace_nowrap` with **one `div().h(px(18))` per line** so height is `lines × 18 + 20 + header` with zero measurement. Copy button absolutely overlaid `top(3) right(5) h(20) px(6) rounded(5)`, hover via `hover_blend → ink(0.08)`, 1200ms "Copied" flash. Syntax highlighting is **pure paint** — tokenized on the background executor, returned as recolored `TextRun`s on the identical mono font, so layout can never depend on it.
- Links: **monochrome by design** — color stays `theme.text`, decorated with a 1px `text_muted` underline. No hover style.
- Blockquote: `border_l_2 accent.opacity(0.6) bg accent.opacity(0.05) rounded_tr/br(6) pl(12) pr(10) py(6)`.
- Tables: frameless "flat hairline" — no outer box, no header fill, no radius, just 1px `hairline(0.10)` rows. Cells `p(12)`, columns `natural = max(content,48) + 24`, `minimum = min(natural, 96)`, whole table `overflow_x_scroll` with `min_w` so it scrolls rather than crushing.
- **No image renderer** — `Tag::Image` folds into the inline link path and renders as underlined alt text.
- Selection: a thread-local paint-ordered registry, cleared by a zero-size canvas painted first. Wash `accent.opacity(0.35)`. Double-click = word, triple = element, drag spans virtualized rows, with quadratic edge auto-scroll (`MAX × t²`, 24ms tick, 36px edge, 24px max step).

**Tool calls.** Consecutive tool parts fold into one group. `CHIP_HEIGHT 38`, `CHIP_GAP 0`, `CHIP_CARD_HEIGHT 30` (so `my(4)` centers a 30px card in a 38px row). A 1px guide rail `ml(12) w(1) bg(ink(0.08))` runs down the group; cards inset past it. Group header `h(26) px(4) gap(8) text_size(12) text_muted, hover → text` with an 18px `rounded(5) bg(ink(0.06))` chevron tile showing `▾`/`▸`. **The header stays `text_muted` even when children failed** — red read as "the whole step broke."

An expandable chip is **one element** whose header row *is* the chip and whose body is the detail — not a floating card below. Card height is an **explicit border-box number**, not intrinsic, because auto-height added 2px per chip and overflowed the group's analytic height. Bodies are ordered invocation-first (what was asked), then output/diff (what came back), each under a 1px `hairline(0.06)` separator. Diffs render through the *real* changes-pane component so an inline tool diff is indistinguishable from the checkout sidebar.

Fold tweens use `RESIZE` (200ms `EASE_OUT`) with the animation key including an epoch, and are **armed only within 400ms of the click** — gpui replays animations on remount, and in a virtualized list every scroll-back-into-view is a remount, so a permanently-armed tween made every collapsed group flash open→closed on reappearance.

**Streaming indicator — two mechanisms**, and the first is the distinctive one:

*(a) A per-chunk fade veil on the text itself* (`markdown/veil.rs`). No shimmer, no blinking cursor, no spinner in the text. Newly appended bytes dissolve in: layout commits instantly and only run **colors** change, with `apply_veil` preserving total run length exactly so shaping and wrapping are byte-identical. Alpha `= 1 − (1−p)^1.6`; per-chunk duration `= clamp(inter-chunk EMA × 3, 120, 400)ms`; EMA `= ema×0.7 + min(gap,1000)×0.3`; fast-stream boost `1 + 0.3 × max(0, active−2)`. Non-append rewrites (`**bol` → `bold`) keep the common prefix's fades and re-veil only the changed tail. Re-entering a streaming session adopts existing text without fading.

Its companion is `markdown/mend.rs`: auto-closes hanging `**`, `*`, `_`, `~~`, backticks and `[text](partial` **in the display parse only**, so a closing marker's arrival never reflows painted text. Between the two, streaming markdown never jumps.

*(b) The working trailer* — in-flow under the last row, so it scrolls with the reply rather than living in a status bar: `gap(8) pt(16)`, a gradient-matrix spinner (2.5px `rounded_full` cells, 750ms wave, driven by a shared 30fps clock with a 300ms lease rather than per-display-frame animation), a rotating flavour word from a 20-word vocabulary seeded by `fnv1a(chat_id)` rotating every 7s, and an elapsed counter at `text_faint`.

**Metadata chrome is minimal**: no token counts, no per-message model badges. Just a hover-revealed timestamp in a **reserved 32px lane** under the entry's last row — reserved so flipping visibility never shifts the virtualizer — at `text_size(12) text_muted.opacity(0.55)`, format `"Jul 1, 3:45 PM"`, with the copy button beside it. No horizontal inset: the label's edge must sit exactly on the text's first-character x, and a `px-1` here caused a reported 4px drift. Assistant rows only get a timestamp once streaming ends ("the turn isn't at a time yet").

### 5b. Composer (`composer.rs`, 6902 lines)

**The text input is hand-rolled**, adapted from gpui's `examples/input.rs` — not a shared framework editor. `ComposerInput` implements `EntityInputHandler` (the IME/system-text-services surface) and is wired to the window from inside `ComposerTextElement::paint` via `window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx)`. The buffer is a plain `String` of raw Markdown; cursor and selection are **byte offsets**; every mutation funnels through `replace_text_in_range` — even Backspace and Cut, which `select_to()` then replace with `""`.

A custom `ComposerTextElement` does measured auto-grow layout via `window.request_measured_layout` and paints shaped lines, caret, selection, mention washes and ghost text in one masked pass.

**Pill chrome** (`composer.rs:5904-5913`):
```rust
div().rounded(px(26.0))
     .bg(theme.input_glass_bg())          // dark: hsla(0,0,1,0.03)
     .border_1().border_color(theme.border)
     .when(!theme.is_frost(), |el| el.shadow_lg())
```
wrapped in `frost::frosted(26.0, 16.0, motion::fade_quick("composer-input", body))`. The pill keeps a **lighter blur (16) than menus (44)**, and the shadow is gated off on frost platforms because a drop shadow behind translucent glass reads as an inner glow. Column: `max_w(768) mx_auto gap(8) px(16) pb(16)`.

**There is no focus styling at all** — no border change, no glow. Focus only affects caret painting.

**Growth** — constants are derived, not magic: `TEXTAREA_PAD_V 20` (pt-4 + pb-1) · `TEXTAREA_MIN 76` · `TEXTAREA_MAX 260` · `ACTIONS_ROW_HEIGHT 46` (pt-1 + 32px chips + pb-2.5) · `PILL_BORDER_V 2` → `COMPOSER_MIN_HEIGHT 124`, `COMPOSER_MAX_HEIGHT 308`. `INPUT_LINE_HEIGHT 22.75` = 14 × 1.625. `composer_total_height(h) = (h + 20).clamp(76, 260) + 46 + 2`. Scrolling starts at 240px of content. Soft wrap comes from passing `Some(width)` to `shape_text`.

There's also a **compact ↔ expanded flip** with hysteresis: newline or capacity <200 always expands; expanded collapses only when `text_width < capacity − 32`. Driven by a manual `FlipMorph` over `COLLAPSE` (180ms `EASE_OUT`) rather than `with_animation`, because element-id keying replays tweens on remount, and it tracks a **live** target so auto-grow mid-morph doesn't finish on a stale height. The pill's bottom edge stays stationary and controls pin to it at full alpha throughout — no fade on the chips.

**Placeholder:** `"Do anything…"`, `theme.text_faint`, same 14/22.75 metrics. Caret still paints at origin while it shows.

**Caret:** 2px wide, full line height, `theme.caret`. Blink is `(ms / 500) % 2 == 0` — solid through the first half-period anchored to the last edit, so a typing burst never blinks. Selection `theme.selection`. The repaint task ticks only while focused *and* the window is active.

**Send button:** a 28px `rounded_full` circle filled `theme.text` with a 14px `ARROW_UP` in `theme.bg`; disabled → `opacity(0.35)` with no cursor or handler; hover → `opacity(0.85)`. Stop is the same circle containing an **11px square at `rounded(3)`** in `theme.bg`. (The doc comment says "red stop square" but the implementation uses `theme.text`/`theme.bg` — doc drift, don't copy.) Mode: no run → Send; run + has content → **Steer**; run + empty → Stop.

**Keybindings** — declared as gpui `actions!(composer, [...])` (36 actions), bound across **two key contexts**. `"Composer"` gets the full set; `"PaletteSearch"` gets text-editing only, with bare arrows and `enter` deliberately **unbound so they bubble** to the palette's own `on_key_down`. That two-context split is the clean seam for reusing one input entity in both a composer and a picker search field.

`enter` Submit · `shift-enter` Newline · `tab` MentionTab · `escape` MentionEscape (propagates when no menu open) · arrows + shift-arrows · `home`/`end` (logical line, not visual row) · `cmd-left/right` → Home/End · `cmd-up/down` → DocStart/DocEnd · `cmd-backspace/delete` → DeleteToLineStart/End · `cmd-z` / `shift-cmd-z` · `alt-` (macOS) / `ctrl-` (elsewhere) word-jump and word-delete · `cmd-a/c/x/v`. **There is no `cmd-enter` binding** and **no shell-style history recall on Up**.

Two notable semantics: `Copy` with no input selection copies the **markdown transcript selection** instead — the composer keeps focus while you read the thread. `Paste` prefers images over text, then file paths, then raw text.

Undo coalesces within 700ms for same-kind contiguous single-character edits, capped at 200 entries; an IME composition is one undo step.

**Mention chips** are the nicest trick here. The raw buffer holds strict Markdown `[basename](zeron-file:percent%2Fpath)`; a `TextProjection` maps it to a display string with `\u{00A0}@label\u{00A0}` and back. `normalize_range` makes each chip **atomic** — a caret inside snaps to the nearer edge, a selection overlapping expands to cover it whole, arrow keys jump it entire. Duplicate basenames get the shortest unique path suffix. Chips paint in `font_mono` at `code_text` over `code_wash` quads at radius 5.

**Toolbar** — actions row `h(46) gap(8) pl(12) pt(4) pb(10)`, utility group `gap(2)` holding the picker chips and the attach button (28px `rounded_full`, `hover_blend → ink(0.10)`, 16px paperclip nudged `left(1)` for optical centering), then the send button. Picker chips are `h(32) max_w(208) gap(6) px(10) rounded(8) text_size(12) MEDIUM`, no border, no caret — and their 32px height is what *defines* `ACTIONS_ROW_HEIGHT`.

**Attachments** — `STRIP_THUMB 56, GAP 8, PAD_TOP 12, PAD_X 16`, flex-wrapped, with `attachment_strip_height()` mirroring the wrap math analytically so the height is known before layout. Thumb `size(56) rounded(8) border_1 hairline(0.10) overflow_hidden`, inner `img` at **explicit `54×54`, `rounded(7)`** (8 minus border) with `ObjectFit::Cover` — explicit dims are required because gpui's `img` honors intrinsic aspect ratio over percent height. Remove button `18px rounded_full` at `top(-6) right(-6)`, `opacity(0)` → `group_hover → 1`, wrapped in `frost::layered(...)` so it draws above the image inside the frosted pill's single layer. Drag-and-drop lives in the **shell**, not the composer: a full-column dropzone with an invisible scrim child revealed by `.drag_over::<ExternalPaths>()`.

**Streaming state:** the input is **never disabled**. No opacity change, no blocked keys, no read-only mode. Only the button morphs — and typing during a live run turns Stop back into Steer. Interrupt is queued on a separate `action_task` from `send_task` so a Stop pressed mid-send doesn't drop the in-flight send future.

**Two completion popups**: `@`-mentions (full-width above the pill, `max_h(320)`, 80ms debounce + one 250ms retry, stale rows kept visible while refining) and `/`-commands (`w(380) max_h(280)`, anchored at the `/` glyph via `visible_point_for_index`, one `LIST_COMMANDS` per harness cached for the composer's lifetime). Both route Up/Down/Enter/Tab/Escape through `sync_mention_controls`, and both have sticky per-token dismissal.

---

## 6. Files worth porting nearly verbatim

Ordered by leverage. The first four are near-zero-risk copies with almost no app coupling.

1. **`crates/ui/src/motion.rs`** (943 lines) — the whole animation system. `CubicBezier` + `MotionSpec` catalog + `fade_in`/`fade_quick`/`menu_in`/`menu_out`/`dialog_in`, the global-keyed `hover_blend`/`hover_listener` (CSS `transition-colors` parity that gpui's snapping `.hover()` can't give you), the shared 30fps pulse clock with lease, `speed_scale`, `reduced_motion`, `lerp`. Depends only on gpui. **Copy this first** — everything else references it.
2. **`crates/ui/src/frost.rs`** (180 lines) — `frosted(radius, blur, child)` and `layered(child)`. Two tiny elements, and the module doc records both non-obvious paint-ordering lessons.
3. **`crates/ui/src/edge_fade.rs`** (185 lines) — `edge_faded()`. The only way to fade content over a blurred backdrop. Builder-shaped (`band_top/bottom`, `fade_overflow_x/y`, `inset_top`), gpui-only.
4. **`crates/ui/src/theme.rs`** (1700 lines) — take the *structure* even if you swap the palette: the `ink`/`hairline`/`wash`/`scrim`/`band` alpha families with dark-quoted alphas and derived light values, the oklch→sRGB converter, `grey(u8)`, and especially the `card_selected_bg` + `card_selected_shadows` inset-ring selection recipe.
5. **`crates/ui/src/popover.rs`** (1150 lines) — `Popup<T>` lifecycle + `reap_popup` (the exit-animation-without-unmount pattern), the six anchor helpers, `popover_card`, `menu_row`/`menu_row_nav`/`menu_heading`/`menu_separator`/`menu_section`, `classify_key`/`menu_step`/`filter_indices`, `skeleton_rows`/`error_row`, the key-cap family, `modal`/`modal_glass`, `dialog_card`. Some app coupling via `Loadable`, but the primitives are clean.
6. **`crates/ui/src/markdown/`** (~4000 lines across 6 files) — parser, renderer, `veil.rs` (streaming fade), `mend.rs` (hanging-marker auto-close), `selection.rs` (pure, gpui-free, directly testable). `veil.rs` + `mend.rs` together are the streaming-markdown quality bar and are independently portable.
7. **`crates/ui/src/composer.rs`** — port the **input half** (`ComposerInput` + `ComposerTextElement` + the `actions!`/`init()` keymap with its two key contexts), not the whole file. `Composer` itself is a ~85-field god-entity mixing pill chrome, morph math, the send RPC pipeline, attachment upload orchestration, two completion popups, a question wizard and a lightbox in one `Render` — don't inherit that shape.
8. **`crates/ui/src/loaders.rs`** (393 lines) — gradient-matrix spinner, pulse skeletons, upload progress ring, all driven by the shared clock rather than per-frame animation.
9. **`crates/ui/src/shell/spaces.rs:1447-2196`** — the add-space palette, as the reference implementation for a modal file/project picker (fixed-height body, band header/footer, breadcrumbs, navigator-first keyboard model).

### Three smells not worth porting

- `menu_row_nav` (`popover.rs:695-708`) paints `selected` and keyboard-`highlighted` **identically** (`card_selected_bg()` for both), contradicting its own doc comment. The model list gets it right — selected = wash + ring, cursor = `ink(0.05)` — and every non-model picker can therefore show two identical-looking rows. Fix on the way in.
- Two parallel search-box designs: `popover::search_input_frame` for branch/space/device vs a bespoke 46px bordered row in the model picker. Same job, different vocabulary. Pick one.
- `FileMentionState` and `SlashState` in `composer.rs` are two near-identical completion state machines (same `token`/`active`/`request`/`loading`/`error`/`dismissed` fields, near-identical `reset_*`/`move_*`/`dismiss_*`/`accept_*`/`render_*` pairs). Extract one generic completion controller when you port it.
