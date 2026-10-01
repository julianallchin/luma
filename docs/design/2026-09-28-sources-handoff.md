# Handoff: every color effect from `color@1` and sources

Date: 2026-09-28. Branch to start from: `patterns-principles` (not merged to
dev). Owner: Julian. Background: `2026-09-27-tonight.md` and the flowcharts
in `2026-09-28-presets-from-color.html` (open it in a browser; the charts are
mermaid).

## Goal

One Color form, `color@1`, makes every shipped color preset. `color.sparkle@1`
and `color.noise@1` are deleted; their presets become saved `color@1` inputs.
No control that exists today may be lost, and no dynamism may be lost.

## Rules (from Julian)

1. **Every number can take a source.** every, life, width, offset, coverage,
   level, scale, speed, contrast, brightness, alpha. If you add a number
   that cannot take a source, that is a bug. Do not add fixed-only numbers
   such as a stroke "count".
2. **Choices are settings.** axis, span, plane, grain, boundary, width
   relative, curve points. They do not animate (they do not today either).
3. **A preset is saved inputs.** No preset may depend on a setting that only
   one form has. Grain is the example of what not to do.

## How it works today (read these first)

- `backend/crates/patterns/src/forms/mod.rs`
  - `color()` (~line 379): one hit clock from the form's `every`
    (`core/odometer` + `core/fraction`). Brightness × alpha goes through
    `core/channel_maximum`, then multiplies color.
  - `stroke()` (~line 460): a Space source with `move` makes one stroke per
    hit, `travel` beats long, via `core/event_life`. Strokes overlap when
    travel > every; there is one channel per live stroke, and the brightest
    stroke wins at each head. Width can be a Time curve (over the clip) or a
    Hit curve (over each stroke's life). `width_relative` makes width a
    share of the gap between strokes (`spacing` output of `event_life`).
  - `hit_progress()` (~line 1528): every Hit source in the clip reads the
    strokes' life when a moving Space source exists, else the hit clock.
    This is how color or width follows each stroke.
  - `clocks()` (~line 1540): an `every` or `travel` given as a Time curve
    becomes a speed curve on the clocks (a chase can speed up).
  - `sparkle()` (~line 635): `event_life(every, duration)` → `random_share
    (index, coverage, grain)` × brightness × present → channel max. Events
    overlap when duration > every (Shimmer: every 1/8, duration 1/2).
    Coverage and brightness take Time/Hit/Noise/Audio; Hit reads the event
    life (Build, Dissolve).
  - `noise()` (~line 718): space noise from u/v position ÷ scale, over an
    odometer of `speed`; `contrast` stretches around 0.5.
- `backend/crates/patterns/src/forms/ops.rs` `event_life()` (~line 861):
  the event tensor (progress, present, index, spacing per live event).
- `backend/crates/patterns/src/sources.rs`: `SourceKind` (Time, Hit, Noise,
  Audio, Space), `SpaceSource` + `Movement` (path, travel, width,
  width_relative, boundary), `NoiseSource` (speed, range: it wanders over
  time only, the same value on every head).
- `backend/crates/patterns/src/presets.json`: the shipped presets (listed
  below).

## Target model

`color@1` inputs: `color`, `brightness`, `alpha`. The form's `every` input
goes. Each input takes a fixed value or one source.

### Events (new shared block)

An events block is `every` (beats) and `life` (beats). An event starts every
`every` beats and lives `life` beats. `every` 0 is one event over the whole
clip. `life` omitted = `every`. Both are numbers, so both take a source (a
Time curve over the clip, as `clocks()` does today). When `life` > `every`,
events overlap.

Who owns events:
- a **Time** source, or a **Random** source, may have its own events block;
- a source nested inside another source (for example in Space `width`, or
  Random `coverage`) with no events block **inherits** the enclosing
  source's events (this replaces `hit_progress()` for width and coverage);
- to follow events across inputs, a Time source takes
  `"events": {"same_as": "<input name>"}` (for example color follows the
  brightness strokes; this replaces the cross-input half of
  `hit_progress()`);
- with no events and nothing to inherit, a Time source is one event over the
  clip (today's Time source).

Overlap: the input is evaluated once per live event (one channel per event,
as today). Brightness and alpha combine with `channel_maximum`. Color: do
what today's graph does when a Hit color follows overlapping strokes; find
out and keep it (open item 1).

### Sources

| Source | Settings (fixed choices) | Number inputs (each takes a source) | Replaces |
|---|---|---|---|
| Time | events or `same_as`; curve points or gradient + curve | every, life | Time and Hit sources, form `every`, sparkle `every`/`duration` |
| Space | axis, span, plane, grain, boundary (clip/wrap), width relative; curve or gradient | offset, width | Space + `move` block. `travel` = life of the offset's Time source, `path` = its curve |
| Random | grain; events or inherit | coverage, level | `color.sparkle@1` (`random_share`) |
| Noise | grain | scale, speed, contrast, range | `NoiseSource` and `color.noise@1`. scale 0 = same value on every head (today's Noise source) |
| Audio | as today | as today | nothing |

Space: with no `offset`, the curve spans the whole axis (today's static
Space). With `offset`, the values are a stroke `width` wide at that position
(today's moving Space). An offset from a Time source with events makes one
stroke per event; strokes overlap when life > every. Keep `boundary` and
`width_relative` exactly as today.

Random: selects `coverage` of the units (at `grain`) again for each event,
keyed by the event index as `random_share` does. Selected units get `level`
(default 1), the others 0. `level` from a nested Time source gives the
flash shape (Shimmer).

Grain: a choice on Space, Random and Noise (head, fixture, clump of 2/4/8,
as Sparkle has today). All heads in one unit take the value at the unit.

### JSON (proposal; keep names short and match the existing style)

```json
{"type": "time", "value": {"every": {"type": "beats", "value": 2},
  "life": {"type": "beats", "value": 4}, "points": [[0, 0], [1, 1]]}}

{"type": "time", "value": {"events": {"same_as": "brightness"},
  "gradient": {...}, "curve": {...}}}

{"type": "space", "value": {"axis": {...}, "curve": {...},
  "offset": {"type": "time", "value": {"every": ..., "life": ..., "points": [[0,0],[1,1]]}},
  "width": {"type": "number", "value": 0.2}, "width_relative": true,
  "boundary": "clip"}}

{"type": "random", "value": {"every": ..., "life": ..., "grain": "head",
  "coverage": {"type": "proportion", "value": 0.3},
  "level": {"type": "time", "value": {"points": [[0,0],[0.15,1],[1,0]]}}}}

{"type": "noise", "value": {"scale": ..., "speed": ..., "contrast": ...,
  "range": [0, 1], "grain": "head"}}
```

## Every shipped color preset in the new model

Start: **Wash** = color white, brightness 100%.

| Preset | color | brightness |
|---|---|---|
| Wash | white | 100% |
| Pulse | white | Time every 1, curve hold 1 → 0.5, fall to 0 |
| Color fade | Time over clip, gradient (was Hit, every 0) | 100% |
| Rainbow | same, rainbow gradient | 100% |
| Gradient | Space axis u, gradient | 100% |
| Chase | white | Space u, flat curve, width 0.2 relative, clip, offset ← Time every 2 life 2 path 0→1 |
| Wave | white | Chase with bump curve, width 1 absolute |
| Ripple | white | Wave on radial axis, width 0.4 relative |
| Spin | white | angle axis, curve ramp, width 0.25 relative, wrap |
| Bounce | white | Wave, every 4 life 4, path 0→1→0, width 0.2 |
| Alternating sides | white | Chase, path held at 0.25 then 0.75, width 0.5 |
| Stepped chase | white | Chase, every 4 life 4, path held in 4 steps, width 0.25 |
| Grow | white | radial, span fixture, every 0 (over clip), path 0→0.5, width 1.25 |
| Random heads | white | Random every 1 life 1, coverage 0.5 |
| Shimmer | white | Random every 1/8 life 1/2, coverage 0.3, level ← Time 0→1 at 0.15→0 |
| Build | white | Random every 4 life 4, coverage ← Time 0→1 (inherits events) |
| Dissolve | white | same, coverage ← Time 1→0 |
| Drift | white | Noise scale 0.5, speed 8, contrast 0.3 |
| Atmosphere | blue, alpha 0.7 | Noise scale 1, speed 16, contrast 0.1 |
| Aurora | green | Noise scale 0.3, speed 4, contrast 0.8 |

Read exact values from `presets.json`; the table is a guide. The old
`color.sparkle@1` preset `brightness` input (a Hit curve, Shimmer) becomes
`level`.

## Must not be lost (each needs a test)

1. Several strokes at once: Chase with life 6, every 2 → 3 live strokes,
   brightest wins.
2. Width over the clip (Time) and width over each stroke (inherited events).
3. `width_relative` against the gap between strokes.
4. Boundary clip (stroke enters and leaves) and wrap.
5. `every` and `travel`/`life` as Time curves (chase speeds up).
6. Coverage and level over each event's life (Build, Dissolve, Shimmer).
7. Coverage from Noise or Audio.
8. Overlapping sparkle events (Shimmer).
9. Color following each stroke (`same_as`).
10. Grain head/fixture/clump on Random; now also on Space and Noise.
11. Two Noise sources in one clip wander apart (the per-input offset in
    `forms/mod.rs` ~line 1586).

## Verification

- For every shipped preset and every stored clip: render old vs new with
  pattern-eval over a range of beats and compare. They must match to float
  tolerance. Audio clips need track analysis; list them and skip.
- The same check ran for the Rec. 2020 migration on 2026-09-28; reuse that
  approach (scratch tool `clipconv`, see the tonight doc status).

## Migration

- Convert stored clips and drafts: `hit` → `time` with events; form `every`
  moves onto the sources that used it; `move` → Space `offset`/`width`/
  `boundary`; `color.sparkle@1` and `color.noise@1` → `color@1`.
- Take backups first (Supabase `backup` schema and a local copy), as on
  2026-09-28.
- **Ask Julian before writing to Supabase.** Local first. The refresh-token
  and host-proof steps for scripts are in memory
  (`reference_supabase_session_outside_app`); a wrong proof breaks sync.
- After the migration, delete the old ids and the load-time conversion, as
  was done for `color.*` on 2026-09-28.

## Places to update

- Engine: `sources.rs`, `value.rs` (Hit), `forms/mod.rs`, `forms/ops.rs`,
  `presets.json`, `catalog.rs`, tests in `backend/crates/patterns/tests/`
  (`color_form.rs`, `forms.rs`, `event_tensors.rs`).
- Backend: `backend/src/node_graph/lighting.rs`,
  `backend/src/migration/`.
- Python: `backend/python/luma_exec/score.py` and its tests.
- UI: `gpui/crates/app/src/track_editor/sheet/form.rs` (source picker,
  chips read "Over clip" or "Every 2 beats · life 4"; events `same_as`
  picker), the curve strip `gpui/crates/ui/src/arg/strip.rs`.
- Agent: `resources/skills/node-cards/SKILL.md`,
  `gpui/crates/agent/tests/js/headless/clip_forms.test.js`,
  `docs/design/agent-code-execution.md`.

## Open items (ask Julian)

1. Which color a head gets when color follows overlapping strokes. Read
   today's graph first and propose keeping it.
2. JSON names (`events.same_as`, `level`, `scale` 0 meaning "no space").
3. Whether aim's fan and motion also move to sources. Not part of this
   work.

## Working rules

- Branch in the main checkout; no worktrees.
- Run only the tests you change or add; no full-suite baseline runs.
- Short, plain English in docs, UI text and commits.
- After a build lands, relaunch the app natively on Wayland:
  `env WAYLAND_DISPLAY=wayland-1 gpui/target/debug/luma-app`.
