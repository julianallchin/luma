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

## Sources

An input takes a plain value, or a source where its card allows it. A source
is one level deep.

- `time` — one curve over the whole clip, all heads equal:
  `{"type": "time", "value": {"points": [[0, 2], [1, 0.5]], "segments": ["linear"]}}`.
- `hit` — one curve over the life of each event (chase and sparkle only).
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

All selected heads one color. `color` (T), `alpha` (T N A).
Preset: **Wash**.

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

- `color` (T), `axis` (default `u`), `every` (beats between strokes, or an
  event list; T), `travel` (beats for one stroke to cross the axis; T),
  `width` (0..4; T H) with `width_relative` (true: share of the gap between
  strokes; false: share of the axis), `shape` (brightness across the stroke),
  `path` (position over the stroke's life), `boundary` (`clip` or `wrap`),
  `alpha` (T H N A).
- Strokes on the axis at once = `travel / every`. With `clip`, a stroke enters
  and leaves fully.

Presets: **Chase**, **Wave** (soft, width abs 100%), **Ripple** (radial),
**Spin** (angle), **Bounce**, **Alternating sides** (x, two steps, travel =
every), **Stepped chase**.

## color.sparkle@1

Each event lights a random share of the heads.

- `color` (T), `every` (beats or event list; T), `duration` (life of one
  event; T), `coverage` (share of heads lit; T H N A), `brightness` (T H N A),
  `grain` (what one head is), `alpha` (T N A).
- A new random set per event, from the clip seed. Overlapping events keep the
  maximum.

| Preset | coverage | brightness |
|---|---|---|
| **Pulse** | 100% | `hit[hold then drop]` |
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

## Layers

Layers combine forms by blend mode. A rainbow that chases is a
`color.time@1` clip with a `color.chase@1` clip above it in `multiply`.
