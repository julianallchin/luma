---
name: node-cards
description: One card per clip form. Every form id, what it does, its inputs with units and the sources each accepts, and its named presets with their values. Read this before placing a clip; use luma.track.definition(form) only for exact types and defaults.
---
# Form cards

A clip plays one form. A clip holds a value for **every** input of its form;
a missing or unknown input fails `edit.check()`. Start from the defaults:

```python
inputs = {key: spec["default"] for key, spec in luma.track.definition(form)["inputs"].items()}
```

Units: `proportion` is 0..1; `beats` are musical beats; colors are RGB in
0..1 or `#RRGGBB`. Every form has `alpha` (proportion): how much the clip
counts. It multiplies brightness and is the clip's opacity. Use it instead of
a clip fade.

A hit is a clip on the timeline. `every` is a fixed period in beats; to
light irregular hits (drum hits), place one clip per hit with `every` 0.

## Sources

An input takes a plain value, or a source where its card allows it. A source
is one level deep.

- `time` — one curve over the whole clip, all heads equal:
  `{"type": "time", "value": {"points": [[0, 2, "ease-out"], [1, 0.5]]}}`.
  A curve is points `[x, value]` or `[x, value, ease]`, x strictly increasing
  from 0 to 1. The ease moves the value to the next point: `"linear"` (the
  default), `"ease-in"`, `"ease-out"`, `"ease-in-out"`, `"hold"` (jump at the
  next point) or `[x1, y1, x2, y2]`, a CSS cubic-bezier local to the segment,
  every number in 0..1. The last point has no ease.
- `hit` — one curve over the life of each event (chase, sparkle, and the
  hits of `color.constant@1`).
- `noise` — `{"type": "noise", "value": {"speed": 4, "range": [0.2, 1]}}`,
  smooth random wandering.
- `audio` — energy of a frequency range of the mix, scaled over the clip:
  `{"type": "audio", "value": {"from_hz": 40, "to_hz": 100, "floor": 0.3}}`.
  Add `"threshold"` (0..1) to gate quiet energy. Ranges: Kick 40–100,
  Bass 20–250, Mids 250–4000, Highs 4000–16000, Full 20–16000.

Segments are `hold`, `linear`, `step` or `bezier`. Promotable key below:
**T** time, **H** hit, **N** noise, **A** audio.

## Axis

`axis` is a mapping. Shorthand strings: `u` (stage right), `v` (downstage),
`z` (up), `order`, `major_axis`, `radial`, `angle`, `vector`. Radial and angle
need a plane; the shorthand gives `{"kind": "auto"}`. The full value is
`{"source": {"kind": K}, "per_group": false, "reverse": false}`, with optional
`"span"` (`fixture` or `group`: each fixture or group gets its own axis),
`"plane"` for radial and angle (`auto`, `up_down`, `front_back`, `left_right`,
`{"kind": "custom", "normal": [u, v, z]}`) and `"mirror"`. An axis has no
reverse; use a backward path. Radial and angle read around the centroid of
the span.

## color.constant@1

All selected heads one color. `color` (T), `brightness` (multiplies the
color; T H N A), `every` (beats between hits; 0 is one hit over the clip; T),
`alpha` (T N A). Only `brightness = hit[...]` reads the hits; each hit lasts
until the next. Brightness darkens the light; alpha is how much the clip
covers the layers under it.
Presets: **Wash** (brightness 1, every 0), **Pulse** (brightness
`hit[hold then drop]`, every 1).

## color.time@1

A color gradient over time, all heads equal. `colors` (gradient), `curve`
(envelope over the clip), `every` (beats; 0 plays the gradient once over the
clip, N repeats it every N beats; T), `alpha` (T N A).
Presets: **Color fade** (every 0), **Rainbow** (hue gradient, every 4).

## color.space@1

A gradient laid across the rig. Each head gets a fixed color from its
position. `colors` (gradient), `axis`, `alpha` (T N A). Preset: **Gradient**.

## color.chase@1

Strokes travel across the heads. Each event starts one stroke.

- `color` (T), `axis` (default `u`), `every` (beats between strokes; T),
  `travel` (beats for one stroke to cross the axis; T),
  `width` (0..4; T H) with `width_relative` (true: share of the gap between
  strokes; false: share of the axis), `shape` (brightness across the stroke),
  `path` (position over the stroke's life), `boundary` (`clip` or `wrap`),
  `alpha` (T H N A).
- Strokes on the axis at once = `travel / every`. With `clip`, a stroke enters
  and leaves fully. `every` 0 is one stroke per clip; `travel` 0 is the whole
  clip.

Presets: **Chase**, **Wave** (soft, width abs 100%), **Ripple** (radial),
**Spin** (angle), **Bounce**, **Alternating sides** (x, two steps, travel =
every), **Stepped chase**, **Grow** (radial, fixture span: each fixture lights
from its middle out to both ends and stays lit; one stroke over the clip).

## color.sparkle@1

Each event lights a random share of the heads.

- `color` (T), `every` (beats; T), `duration` (life of one
  event; T), `coverage` (share of heads lit, below 1; T H N A), `brightness`
  (T H N A), `grain` (what one head is), `alpha` (T N A).
- Sparkle is random heads only. A fixed coverage of 1 is rejected: all heads
  on each hit is a `color.constant@1` Pulse.
- A new random set per event, from the clip seed. Overlapping events keep the
  maximum.

| Preset | coverage | brightness |
|---|---|---|
| **Dissolve** | `hit[100% → 0%]` | 100% |
| **Build** | `hit[0% → 100%]` | 100% |
| **Random heads** | 50% | 100% |
| **Shimmer** | 30%, every 1/8 beat | `hit[spike]` |

## color.noise@1

Soft brightness that wanders across space and time. `color` (T), `speed`
(beats; T), `scale` (blob size, share of the rig; T), `contrast` (T N A),
`alpha` (T N A). Presets: **Drift**, **Atmosphere**, **Aurora**.

## strobe.constant@1

Fixture shutter strobe. Writes only the strobe channel, so it strobes what
the layers under it light. The rate is `rate × alpha`. `rate` (T N A),
`alpha` (T N A). Preset: **Strobe**. A colored strobe is a color clip with a
strobe clip above it.

## aim@1

Where the heads of a clip point: a rest aim (or a point every head points
at), an optional fan across the axis, and optional motion around it. It
takes two blend modes:

- `replace` (default): the clip blends toward its own aim by `alpha`, so
  stack aim clips to move a subset of heads without resetting the rest.
- `offset`: `base`, `direction` and `point` are not used. The fan and motion
  turn the aim under the clip, per head, per frame; `alpha` scales their
  degrees. Over no aim it starts from the head's home. Offset clips stack:
  a `replace` Position, an `offset` Circle above it and an `offset` Fan above
  that give a fanned circle around the position. One movement clip in
  `offset` runs over many positions.

No other blend mode is valid for aim, and no color or strobe clip takes
`offset`.

- `base` (choice: `direction`, `point`) — whether the rest aim is one
  direction or a point every head points at.
- `direction` (vector U/V/Z; T N), used when `base` is `direction`.
- `point` (vector U/V/Z in metres; T), used when `base` is `point`.
- `fan` (degrees the heads spread apart across the axis, 0 = all alike; T H N A).
- `axis` (how the heads are laid out, for `fan` and `spread`; same shorthand
  as [Axis](#axis)).
- `motion` (choice: `none`, `shape`, `noise`) — whether the heads wobble
  around the base.
- `shape` (choice: `swing_left_right`, `swing_up_down`, `circle`,
  `figure_8`), used when `motion` is `shape`.
- `size` (degrees of the wobble; T A).
- `every` (beats for one wobble cycle, and between hits; T).
- `spread` (degrees of phase the wobble travels across the heads; 360 = one
  cycle, 0 = all together; T).
- `speed` (beats for `noise` motion to wander one step; T), used when
  `motion` is `noise`.
- `alpha` (in `replace`, the blend toward this clip's aim; in `offset`, the
  share of its fan and motion degrees; T H N A).

Presets: **Position** (rest direction, no fan or motion), **Fan** (40°
static fan), **Converge** (`base` point, heads aim at one point), **Bloom**
(fan grows 0→40° over the clip on a radial axis), **Sweep** (45° swing
left-right over 8 beats), **Wave** (25° swing up-down, spread across the
heads), **Circle** (18° circle wobble), **Figure-8** (25° figure-8 wobble),
**Ballyhoo** (40° noise wobble).

## Layers

Layers combine forms by blend mode. A rainbow that chases is a
`color.time@1` clip with a `color.chase@1` clip above it in `multiply`.
