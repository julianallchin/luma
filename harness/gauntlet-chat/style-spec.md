# Comet (Zeron) style spec — design source for Luma's GPUI agent chat

Every value below is copied from the comet clone at
`…/scratchpad/comet`. Paths are relative to that clone root; `file:line`
is the authority for each number. This spec is the *visual bar* a critic
should measure our chat against — it is not a mandate to copy the code.

Comet is MIT (`LICENSE:1`). Source reuse with attribution is permitted.

---

## 0. Two appearances, one token set

`crates/ui/src/theme.rs:1-35` — dark is canonical; light is **designed, not
inverted**. Three rules it states explicitly and we should inherit:

1. Surface order flips meaning. Dark: content panel is the *darkest* plane,
   raised surfaces get *lighter*. Light: content panel is *white*, chrome goes
   *grey* — chrome recedes by darkening.
2. Elevation reverses. Dark lifts with a faint white wash; light lifts with
   white + border + shadow. Fill *alphas* carry over unchanged
   (`INK_FILL_SCALE = 1.0`, `theme.rs:138`); only hairlines scale
   (`INK_HAIRLINE_SCALE = 1.35`, `theme.rs:144`).
3. Accents move down the scale: dark uses oklch 400-level, light the 600-level
   sibling at identical hue, so the *contrast ratio* is preserved.

Motto from the module docs, worth stealing verbatim: **numbers drive layout,
colors are paint.** Layout constants never depend on which color is painted.

---

## 1. Color

### 1.1 Dark palette (`theme.rs:428-476`, `Theme::dark()`)

Surfaces are sampled from screenshots of the original app, not generated.

| Token | Value | Note |
| --- | --- | --- |
| `bg` | `grey(6)` = `#060606` | main content panel |
| `surface` | `grey(13)` = `#0d0d0d` | shell / sidebar |
| `surface_raised` | `neutral(0.235)` | |
| `surface_card` | `grey(0x0e)` | |
| `surface_dialog` | `grey(0x10)` | |
| `surface_overlay` | `grey(0x16)` | |
| `element_hover` | `hsla(0, 0, 0.92, 0.11)` | |
| `element_active` | `hsla(0, 0, 0.92, 0.16)` | |
| `border` | `hsla(0, 0, 1.0, 0.08)` | |
| `border_strong` | `hsla(0, 0, 1.0, 0.14)` | |
| `text` | `neutral(0.922)` | ~neutral-200 |
| `text_muted` | `neutral(0.708)` | ~neutral-400 |
| `text_faint` | `neutral(0.556)` | ~neutral-500 |
| `input_bg` | `hsla(0, 0, 1.0, 0.03)` | composer plate |
| `selection` | `hsla(0.66, 0.6, 0.55, 0.35)` | |
| `caret` | `hsla(0.66, 0.7, 0.7, 1.0)` | |
| `accent` | `oklch(0.673, 0.182, 276.935)` | indigo-400 |
| `accent_strong` | `oklch(0.585, 0.233, 277.117)` | indigo-500 |
| `danger` | `oklch(0.704, 0.191, 22.216)` | red-400 |
| `warning` | `oklch(0.828, 0.189, 84.429)` | amber-400 |
| `success` | `oklch(0.765, 0.177, 163.223)` | emerald-400 |
| `busy` | `oklch(0.718, 0.202, 349.761)` | pink-400 |
| `code_text` | `oklch(0.811, 0.111, 293.571)` | violet-300 |
| `code_wash` | violet-400 @ 12% | inline-code pill |
| `diff_add` / `diff_del` | emerald-400 / red-400 | |
| `diff_hunk_bg` | `hsla(0.6, 0.35, 0.6, 0.05)` | |

Constructors: `grey(u8)` (`theme.rs:984`), `neutral(l)` (`theme.rs:803`),
`oklch(l, c, h_deg)` (`theme.rs:989`).

### 1.2 Context-free paint helpers (`theme.rs:818-895`)

These are the reason the palette stays small. Called from deep inside element
builders with no `cx`; they read a process-wide appearance mirror
(`CURRENT_APPEARANCE`, `theme.rs:78`).

- `ink(a)` — dark `white @ a`, light `black @ a * INK_FILL_SCALE`
- `hairline(a)` — dark `white @ a`, light `black @ min(a*1.35, 0.5)`
- `wash(a)` — dark `hsla(0,0,0.92,a)`, light `hsla(0,0,0.10,a)`
- `scrim(a)` — modal backdrop; `SCRIM_ALPHA_DARK = 0.60` (`theme.rs:863`),
  light `0.32` scaled proportionally
- `band()` — dark `black @ 0.16`, light `black @ 0.045`

Anything that caches a *resolved* `Hsla` (notably the markdown `TextRun`
cache) must key on `theme_generation()` (`theme.rs:84`) and drop on change.

### 1.3 Glass / vibrancy (`theme.rs:428-600`)

- `GLASS_ALPHA = 0.80` on macOS, `1.0` elsewhere (`theme.rs:435`). Linux/Windows
  get no compositor-blur guarantee, so they are opaque — a merely *transparent*
  window shows raw desktop through the sidebar.
- `GLASS_ALPHA_LIGHT = 0.80` on macOS (`theme.rs:445`).
- `glass()` (`theme.rs:480`) — dark: `grey(8) @ 0.80`. Light: `grey(0xfa) @ 0.80`
  (deliberately *not* the surface grey — at 80% coverage the tint IS the sidebar
  tone, and the darker grey read dingy next to the white content card).
- `glass_hover()` (`theme.rs:532`) — dark `wash(0.11)`, light `wash(0.06)`.
- `glass_overlay()` (`theme.rs:550`) — floating-card tint over the blur.
  Dark: `oklch(0.33, 0, 0) @ 34%`. Light: `surface_overlay @ 85%`.
  History worth knowing: 65% coverage buried the backdrop and menus read as
  flat grey slabs; 34% lets the blur carry the card.
- `input_glass_bg()` (`theme.rs:563`) — over glass, light thins to
  `input_bg @ 0.30`; dark's 3% white wash is already glass-native.
- `card_glass_bg()` (`theme.rs:575`) — `surface @ 0.40` on glass.
- `user_bubble_bg()` (`theme.rs:918`) — dark `wash(0.08)`, light `wash(0.04)`.
  Translucent so the vibrancy reads through; an opaque plate read as a slab.
- `glass_selected_bg()` / `card_selected_bg()` (`theme.rs:907`, `:930`) —
  dark `wash(0.11)`, light `wash(0.06)`.

Window compositing: `window_background_appearance()` (`theme.rs:599`) returns
`WindowBackgroundAppearance::Blurred` only for dark macOS, `Opaque` otherwise.
**It must be re-applied after every theme swap** — gpui's macOS backend tears
the `NSVisualEffectView` out of the hierarchy whenever the value is anything
but `Blurred` (`appearance.rs:186-235`, `lib.rs:248`).

---

## 2. Geometry

### 2.1 Radii (`theme.rs:464-468`)

| Name | px | Applies to |
| --- | --- | --- |
| `BUBBLE_RADIUS` | 16 | user message bubble |
| `PANEL_RADIUS` | 10 | panels / cards |
| `CONTROL_RADIUS` | 6 | buttons, chips |
| `popover::CARD_RADIUS` | 12 | floating menu card (`popover.rs:305`) |

### 2.2 Spacing scale (`theme.rs:470-473`)

`SPACE_XS 4` · `SPACE_SM 8` · `SPACE_MD 12` · `SPACE_LG 16`.

### 2.3 Chrome heights (`theme.rs:447-462`)

| Name | px |
| --- | --- |
| `TITLEBAR_HEIGHT` | 38 |
| `TITLEBAR_TOP_PAD` | 2 (content rides low so air above matches the gap below) |
| `HEADER_HEIGHT` | 44 |
| `STATUS_STRIP_HEIGHT` | 24 (reserved, so the composer never shifts) |
| `TRANSCRIPT_FADE_BAND` | 24 |

### 2.4 Transcript (`transcript.rs:56-131`)

| Name | px | Note |
| --- | --- | --- |
| `MAX_CONTENT_WIDTH` | 736 | the 46rem reading column |
| `GAP_TURN` | 14 | between turns |
| `GAP_BLOCK` | 8 | within a turn |
| `OVERDRAW_PX` | 320 | `ListState` overdraw |
| `STICK_THRESHOLD_PX` | 70 | re-engage bottom pin |
| `SCROLL_BUTTON_THRESHOLD_PX` | 320 | show jump-to-bottom |
| `CHIP_HEIGHT` / `CHIP_CARD_HEIGHT` / `CHIP_GAP` | 38 / 30 / 0 | tool chip on a continuous rail |
| `ATT_THUMB_W` × `ATT_THUMB_H` | 112 × 80 | attachment thumbs, fixed strip height |

### 2.5 Composer (`composer.rs:46-84`)

| Name | px | Note |
| --- | --- | --- |
| `TEXTAREA_PAD_V` | 20 | |
| `TEXTAREA_MIN` / `TEXTAREA_MAX` | 76 / 260 | grow range |
| `ACTIONS_ROW_HEIGHT` | 46 | |
| `PILL_BORDER_V` | 2 | |
| `COMPOSER_MIN/MAX_HEIGHT` | 124 / 308 | derived sums |
| `COMPACT_TOTAL_HEIGHT` | 49 | |
| `COLLAPSE_HYSTERESIS` | 32 | prevents flapping |
| `STRIP_THUMB` / `STRIP_GAP` / `STRIP_PAD_TOP` / `STRIP_PAD_X` | 56 / 8 / 12 / 16 | |

---

## 3. Typography

Fonts: **Geist** (sans) and **Geist Mono**, embedded via `include_bytes!`
(`lib.rs:51-61`), SIL OFL 1.1. Static 500/600/700 faces ship *alongside* the
variable TTF because gpui's cosmic-text path (Linux) rasterizes variable fonts
at their default instance only — weights silently paint at 400 otherwise
(`lib.rs:56-61`). System fallbacks are configured as `font_sans_fallback` /
`font_mono_fallback` (`theme.rs:473-476`).

Markdown scale (`markdown/render.rs:29-60`):

| Name | value |
| --- | --- |
| `MD_TEXT_SIZE` / `MD_LINE_HEIGHT` | 14 / 22 |
| `MD_BLOCK_GAP` | 12 |
| `CODE_TEXT_SIZE` / `CODE_LINE_HEIGHT` | 12.5 / 18 |
| `CODE_PADDING_X` / `CODE_PADDING_Y` | 12 / 10 |
| `TABLE_CELL_PADDING` | 12 (uniform) |
| `TABLE_DIVIDER` | 1 (hairline, `hairline(0.10)`) |
| `TABLE_HEADER_WEIGHT` | 700 |
| `TABLE_MIN_COLUMN_WIDTH` / `TABLE_MIN_COLUMN_CONTENT` | 96 / 48 |

Tables are **frameless**: horizontal hairlines under the header and between
rows are the only chrome — no outer box, no header fill, no radius
(`render.rs:39-45`).

Chrome scale observed in `transcript.rs`: body 14/22 (`:3112`), secondary 12
(`:2931`), meta 11–11.5 (`:2980`, `:4097`), micro labels 10–10.5
(`:3594`, `:3756`).

Composer input: `INPUT_TEXT_SIZE 14`, `INPUT_LINE_HEIGHT 22.75`
(`composer.rs:70-71`).

---

## 4. Motion

`crates/ui/src/motion.rs` is a complete, self-contained animation kit over
gpui's `Animation`/`AnimationExt`. Read its module doc (`motion.rs:1-28`) — it
is the single best artifact in the repo.

### 4.1 Easing curves (`motion.rs:211-220`, `:310`)

`CubicBezier` (`motion.rs:136-205`) evaluates CSS `cubic-bezier()` exactly and
converts to gpui's `Fn(f32) -> f32` easing shape.

| Const | Bezier | CSS name |
| --- | --- | --- |
| `EASE_OUT_EXPO` | `(0.16, 1, 0.3, 1)` | the signature entrance curve |
| `EASE_OUT` | `(0, 0, 0.58, 1)` | ease-out |
| `EASE` | `(0.25, 0.1, 0.25, 1)` | ease |
| `EASE_IN_OUT` | `(0.42, 0, 0.58, 1)` | ease-in-out |
| `EASE_RESORT` | `(0.22, 1, 0.36, 1)` | list reordering |
| `EASE_TAILWIND` | `(0.4, 0, 0.2, 1)` | hover fades |

### 4.2 The catalog (`motion.rs:283-317`)

`MotionSpec::new(duration_ms, curve)`, optional `.with_delay(ms)` folded into
the timeline because gpui `Animation` has no native delay (`motion.rs:227-246`).

| Spec | ms | curve | use |
| --- | --- | --- | --- |
| `FADE_IN` | 500 | EASE_OUT_EXPO | entrances (+ translateY 4→0) |
| `FADE_QUICK` | 150 | EASE | quick swaps |
| `MENU_IN` | 140 | EASE | popover open |
| `MENU_OUT` | 100 | EASE | popover close |
| `DIALOG_IN` | 180 | EASE | dialog open |
| `SPLASH_OUT` | 500 + 150 delay | EASE | boot splash |
| `RESIZE` | 200 | EASE_OUT | sidebar / pane width+height |
| `TAB_SLIDE` | 150 | EASE_OUT | tab reorder |
| `COLLAPSE` | 180 | EASE_OUT | fold |
| `CHEVRON` | 200 | EASE | disclosure rotation |
| `SCROLL_GLIDE` | 500 | EASE_IN_OUT | scroll-to-row |
| `HOVER_FADE` | 150 | EASE_TAILWIND | hover color blend |
| `ZERON_PULSE` | 2400 | EASE | loader cell wave |
| `GRADIENT_SPIN` | 750 | EASE | working indicator |

### 4.3 Rules the catalog enforces

- **translateY is a relative-position `top` inset**, not a transform: taffy
  applies relative insets after layout, so siblings never move
  (`motion.rs:24-27`). gpui at these revs has no `div` scale transform, so
  `menu-in`/`dialog-in` approximate their scale with fade + translate.
- **Reduced motion is automatic.** `App::reduce_motion` is honored by every
  `with_animation` element: oneshots snap to the end state, repeats to the
  start, no frames scheduled (`motion.rs:18-22`, `set_reduced_motion` `:634`).
- **Repeating loaders do not use `with_animation`.** A single shared 30fps
  `PulseClock` global (`PULSE_TICK = 33ms`, `motion.rs:53`) drives every
  spinner, with a 300ms lease per view (`PULSE_LEASE`, `:58`). One
  `with_animation` spinner requested a redraw every display frame and pinned an
  M-series laptop at 120Hz / **36% CPU**. All cells share one epoch so
  multi-instance loaders stay phase-locked (`pulse_delta`, `:83`).
- **Hover is a blended tween, not a CSS class.** `hover_blend(key, rest, hover)`
  (`motion.rs:608`) + `hover_listener(key)` (`:562`) drive a keyed 150ms
  color lerp. Same mechanism for text and background.
- `ZERON_MOTION_SCALE` env knob via `speed_scale()` (`motion.rs:620`).

### 4.4 Sliding panels (`shell.rs:479-493`, `:2853-2884`)

The pattern is a **manually driven tween**, not `with_animation`:

```rust
struct WidthTween { from: f32, to: f32, started: Instant }

fn eval_tween(&self, tween: Option<WidthTween>, target: f32) -> f32 {
    // reduced motion, absent, or elapsed>=1.0  -> exactly `target`
    // mid-flight -> lerp(from, to, RESIZE.progress(raw)), and set
    //               self.motion_active so render schedules the next frame
}
```

The container clips a **fixed-width inner** so content never reflows during the
transition (`pane_container`, `shell.rs:2869-2883`):

```rust
div().h_full().flex_none().overflow_hidden()
     .w(px(self.eval_tween(tween, target)))
     .child(inner)   // inner is laid out at its final width
```

A live drag sets `sidebar_tween = None` and tracks the pointer directly
(`shell.rs:1967`). The macOS traffic-light spacer tweens on the same clock
(`titlebar_spacer`, `shell.rs:2888`).

### 4.5 Popover open/close (`popover.rs:62-64`, `:340-384`)

Close is a two-phase state machine — `begin_close()` keeps the content mounted
and plays `MENU_OUT` for 100ms, then `finish_close()` unmounts. The exit
animation runs under a *fresh* element id (`format!("{id}-out")`) because
same-id reuse would resume the entrance's timeline (`popover.rs:374-382`).
The backdrop blur radius is scaled by `(1 - exit_t)` so the frost dissolves
with the card (`popover.rs:368`).

---

## 5. Streaming text — the part that matters most

Comet's chat feels good because the streaming path is designed so **layout
commits instantly and only paint animates**. Four cooperating pieces:

### 5.1 Block-incremental parse (`markdown/parser.rs:1-12`, `:552-712`)

`IncrementalParser::append(delta)` reparses only from the **last stable
top-level block boundary** — text before the start of the last top-level block
cannot be affected by an append. Cost is O(delta + last block), not
O(document). Soundness guard: a source containing a link-reference definition
(`[label]: url`) has non-local effects and drops to full reparses
(`parser.rs:563`). Parity tests stream corpora through both paths and assert
equality.

### 5.2 Hanging-marker mending (`markdown/mend.rs`, 413 lines)

`display_tree()` (`parser.rs:592`) auto-closes hanging inline markers
(`**bold`, `[link](url…`) in the **streaming display parse only**, so the
closing marker arriving later never reflows already-painted text. The canonical
parse settles honestly on completion.

### 5.3 The veil — per-chunk opacity fade (`markdown/veil.rs`, 508 lines)

Streamed text is committed to layout immediately; a purely cosmetic veil
dissolves over the newly arrived characters. Constants (`veil.rs:33-42`):

| Name | value |
| --- | --- |
| `VEIL_EMA_SEED_MS` | 160 |
| `VEIL_MIN_FADE_MS` / `VEIL_MAX_FADE_MS` | 120 / 400 |
| `VEIL_CURVE_POW` | 1.6 |
| gap clamp feeding the EMA | 1000 ms |

- duration per chunk: `clamp(ema * 3, 120, 400)` (`veil_duration_ms`, `:66`)
- EMA update: `ema*0.7 + min(gap,1000)*0.3` (`veil_ema_next`, `:77`)
- text alpha: `1 - (1-p)^1.6` (`veil_opacity`, `:59`)
- backlog boost: `1 + 0.3 * max(0, chunks-2)` (`veil_boost`, `:72`)
- **zero translate** — opacity only. No positional offset on streamed content.

Why this is layout-safe (`veil.rs:14-18`): the fade is applied by multiplying
alpha into the `TextRun` colors covering each chunk. A **color-only run split
cannot change layout** — gpui shapes through cosmic-text, whose
`Attrs::compatible` ignores color and metadata, so adjacent same-font runs are
shaped as one contiguous word across the split. Kerning and ligatures survive;
wrapping is byte-identical to the unsplit render.

### 5.4 Cross-frame render cache + single-row remeasure

`RenderCache` (`markdown/render.rs:73-77`) keys settled blocks' flat text and
shaped runs across frames, so a fading live row costs O(tail block), flat in
total reply length.

The transcript is a stock `gpui::ListState` with `OVERDRAW_PX = 320`
(`transcript.rs:1588`). Per delta it calls `remeasure_items(last..last+1)` —
**only the last row** (`remeasure_last_row`, `transcript.rs:1726`). Everything
else keeps its cached height. Tool-chip fold heights are computed *analytically*
(`CHIP_HEIGHT` × count) rather than measured (`transcript.rs:1070`), and code
blocks render per-line so height is exactly `lines × CODE_LINE_HEIGHT`
(`render.rs:4-7`) — syntax highlighting then arrives as recolored `TextRun`s on
the identical mono font and never touches layout.

### 5.5 Stick-to-bottom spring (`transcript.rs:88-113`)

Not a scroll-to-bottom call — a fixed-timestep spring integration:

| Name | value |
| --- | --- |
| `SPRING_DAMPING` | 0.7 |
| `SPRING_STIFFNESS` | 0.05 |
| `SPRING_MASS` | 1.25 |
| `SPRING_FRAME_MS` | 1000/60 |
| `SPRING_MAX_CATCHUP_FRAMES` | 8 (a hitch catches up, never teleports) |
| `SPRING_GROWTH_EMA` | 0.12 (feed-forward target growth) |
| `SPRING_CHASE_MAX_LEAD` | 32 px above true bottom while streaming |
| `AT_BOTTOM_PX` | 2 |
| `SPRING_SETTLE_GRACE_MS` | 500 (stay warm so a pause resumes at cruise) |
| `GLIDE_MAX_VIEWPORTS` | 2.5 (farther than this teleports, then glides) |

The feed-forward growth term is why the tail stays readable instead of hugging
a moving edge.

---

## 6. Effects that need renderer support

| Effect | API | At our gpui pin (32a0e81)? |
| --- | --- | --- |
| Window vibrancy | `WindowBackgroundAppearance::Blurred` via `Window::set_background_appearance` | **Yes** (`window.rs:2596`) |
| Per-element backdrop blur | `Window::paint_backdrop_blur` (`frost.rs:87`) | **No** |
| Per-pixel scroll-edge fade | `gpui::EdgeFade` + `Window::with_edge_fade` (`edge_fade.rs:170`) | **No** |
| Single-layer paint scope | `Window::paint_layer` (`frost.rs:86`) | **Yes** (`window.rs:3952`) |

`frost.rs` (180 lines) exists because of an ordering bug worth knowing about:
with per-primitive bounds-tree ordering, a hover repaint elsewhere could
reassign a card's quads *below* the blur, and washes/dividers/borders got
blurred away. Wrapping the whole subtree in one `paint_layer` makes the
blur→content relationship structural (`frost.rs:4-9`).

`edge_fade.rs` (185 lines) exists because over a see-through blurred backdrop
**no painted overlay can fade content out** — "what is behind the window" is
not a paintable color. It fades per-glyph by distance to the wrapper's own
edges, with `band_top`/`band_bottom` asymmetry for chrome of different heights,
and auto-disables an edge when the scroll handle says there is no overflow that
way (`edge_fade.rs:145-160`).

Blur sigma: `MENU_BLUR = 44.0` for floating cards (`frost.rs:24`); the composer
pill uses 16 (`shell.rs:5258`).

---

## 7. Reference screenshots

Copied into this directory as `comet-ref-*`:

| File | Source |
| --- | --- |
| `comet-ref-main-shell.png` | `docs/screenshot.png` |
| `comet-ref-landing-hero.jpg` | `apps/landing/public/assets/app-screenshot.jpg` |
| `comet-ref-transcript.png` | `docs/media/registry-sync/03-transcript-synced.png` |
| `comet-ref-tool-run.png`, `comet-ref-tool-details.png` | `docs/media/acp/` |
| `comet-ref-fade-timestamp.png` | `docs/media/fade-timestamp/03-before-after.png` |
| `comet-ref-changes-expand.png` | `docs/media/changes-expand-buttons/before-after.png` |
| `comet-ref-working-overlay.png` | `docs/media/send-pending-overlay/` |
| `comet-ref-settings.png` | `docs/media/harness-settings/s2-agents-settings.png` |
| `comet-ref-original.png` | `docs/reference/original-comet.png` (comet's own design source) |
