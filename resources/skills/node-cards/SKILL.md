---
name: node-cards
description: One card per graph node. Every node id, what it does, its inputs with units and defaults, its outputs, and the gotchas. Read this instead of dumping definitions when composing a graph; use luma.track.definition() only for a node's exact JSON body.
---
# Node cards

Every node the library ships, grouped by job. Format per node:
inputs as `name: type = default`, then outputs, then notes. Rates: **fixed**
inputs resolve once per clip; the rest resolve every frame and can differ per
head. Units: `proportion` and `position` are 0..1; `beats` are musical beats;
`degrees`, `seconds`, `number` are plain. A `mask` output is a proportion
signal. Inputs with no default are required.

Two names you may see in older text do not exist: `appearance` and
`add_lighting`. Color a mask by multiplying it with a color. Combine
capabilities by wiring several ports into one `output` node. Layers combine
across clips by blend mode, not inside a graph.

## Mapping (read this first)

A `mapping` value is `{"source": {"kind": K, ...}, "per_group": bool, "reverse": bool}`
or the shorthand string `"u"`, `"v"`, `"z"`, `"order"`, `"major_axis"`, `"circle"`.

- Kinds: `z` up, `u` stage right, `v` downstage, `order` selection index (no
  spatial meaning), `major_axis` principal axis with `toward: [u, v, z]` to fix
  the sign, `circle` best-fit circle with `origin` as a phase offset in turns
  (errors if a head is off the circle), `vector` with `direction: [u, v, z]`.
- Position is min/max normalized to 0..1 over the heads in the clip. Heads all
  at one value land at 0.5.
- `reverse` gives `1 - position`. For `circle` it reverses the winding.
- `per_group` splits the normalization by cell group. A clip has one selection
  and the host gives every head in it the same group, so **inside one clip
  `per_group` changes nothing**. To normalize height within each tower, place
  one clip per tower group.
- Optional `mirror: {"normal": [u, v, z], "offset": metres}` folds heads across a
  plane before mapping. Not allowed with `order`.
- Only `circle` positions wrap. Nodes with a `boundary` input use `natural`
  (wrap only if the mapping wraps), `clip`, or `wrap`.

## Audio

**band_energy** — energy in a frequency band of the analyzed track.
`source: audio_source = mix` (mix, bass, drums, vocals, other), `low_hz = 20`, `high_hz = 60`, both fixed.
Out `value: number`, nonnegative, not bounded.

**band_mask** — band energy times gain, through a shape, as a 0..1 mask.
`gain: number = 10`, `low_hz = 20`, `high_hz = 60`, `source = mix`, `shape: envelope = linear`.
Out `mask`. Gain is a raw multiplier with no fixed scale. Find it by measuring:
sweep gain and read the lit fraction at a loud and a quiet second. The shape
clamps the result to 0..1. A steep shape gives a threshold feel.

**band_pulse** — `band_mask` times a color. Same inputs plus `color = white`. Out `color`.

**drum_trigger** — onset events for one drum. `drum: drum = kick` (kick, snare, hihat, cymbal). Out `trigger: events`.

**drum_time** — time since the last onset of one drum. `drum = kick`. Out `elapsed: beats`, `index: number`, `present: proportion`.

**drum_mask** — a `pulse` retriggered by one drum. `drum = kick`, `duration: beats = 0.5`, `shape = fade out`. Out `mask`.

**drum_pulse** — `drum_mask` times a color. Adds `color = white`. Out `color`.

**audio_spectrum** — full magnitude spectrum, one channel per bin. `source = mix`, `hold_edges: bool = false`. Out `spectrum`, `bin_hz`.

**audio_lowpass / audio_highpass** — filter an audio source before `band_energy`. `source = mix`, `cutoff_hz = 200`. Out `source: audio_source`. Chains up to 64 stages.

**harmony** — detected pitch class. Out `pitch_class: number` 0..11, `present: proportion`.

**harmony_color** — samples a 12 stop gradient by pitch class. `gradient` (C through B). Out `color`. Black until harmony is detected.

## Spatial

**mapped_position** — each head's mapped position. `mapping = z`. Out `value: number` 0..1.
The standard "where is this head" node. Wire it into `core/greater`, `sample_gradient`, or `core/noise`.

**spatial_gradient** — color by mapped position. `gradient = black to white`, `mapping = z`. Out `color`.

**resolve_mapping** — a mapping as a `coordinates` value for `pill`, `chase`, `coordinate_offset`. `mapping = z`. Out `coordinates`.

**coordinate_offset** — signed distance from each head to a query position along a mapping. `mapping: coordinates`, `position: position = 0`, `boundary = natural`. Out `value: number`, `wrapped: mask`.

**pill** — a soft stroke of `width` centred at `position`. `mapping: coordinates`, `position = 0`, `width: proportion = 0.25`, `shape = soft edges`, `active: proportion = 1`, `boundary = natural`. Out `mask`. Width 1 or more lights everything.

**chase** — each trigger launches one stroke from `start` to `end`. `trigger: events`, `mapping: coordinates`, `start: position = 0`, `end = 1`, `travel: beats = 2`, `path: envelope = ramp`, `width = 0.25`, `shape = soft edges`, `boundary = natural`. Out `mask`. Overlapping strokes take the max. The stroke enters from before `start` and leaves past `end` at open edges.

**motion** — position from `start` to `end` over `travel` beats. `elapsed: beats = 0`, `start = 0`, `end = 1`, `travel = 2`, `path = ramp`. Out `position`, `progress`, `active: mask`.

**radial_distance** — metres from the selection's mean U, V. No inputs. Out `value: number`. Ignores Z.

**stage_coordinates** — raw U, V, Z in metres. Out `u`, `v`, `z`.

**fixture_geometry** — raw world XYZ and index. Out `position` (3 channels), `index`.

**circle** — a phase in turns to a `[sin, cos]` pair. `phase = 0`. Out `value` (2 channels). Not a circle fit.

**wander_points** — `count = 6` drifting 3D points inside `minimum..maximum`. `time: seconds`, `seed`, `drift = 0.05`, `amplitude = 0.16`, `periods = [17, 13, 11]`. Out `points` (3 x count channels).

**proximity_weights** — softmax weights from each head to moving points. `position: xyz`, `points`, `temperature = 0.3`. Out `weights`, one channel per point, sums to 1. Temperature 0 is nearest point only.

**core/fit_circle** — phase in turns on a best-fit circle. `position`, `order = 0`. Out `phase`. Falls back to centroid angle if the fit fails.

**core/radial_coordinates** — `phase` (turns) and `radius` around the centroid, from the first two components of `position`.

**core/principal_direction** — 2D principal axis of the selection. `position` (2 channels). Out `direction` (2 channels), one global vector.

**core/rank_nearby** — rank by `value`, merging positions within `tolerance` into one rank. `value`, `order = 0`, `position`, `tolerance = 0`. Out integer `value`.

**core/domain_index** — each head's index in the frame. `value` (any). Out `value`.

**core/align_domain** — reindex `value` onto `reference`'s heads. `value`, `reference`, `order = 0`. Out `value`.

## Masks and shaping

**envelope** — sample a curve at a progress. `shape: envelope`, `progress: proportion = 0`. Out `value: proportion`. Clamps outside 0..1. Knots are `[[x, y], ...]` in 0..1.

**sample_field_envelope** — same as `envelope` with a per-head `phase`. `shape = soft edges`, `phase`. Out `mask`.

**soft_edges** — builds a trapezoid envelope from one knob. `softness: proportion = 0.1`. Out `shape: envelope`. 0 is a hard rectangle, 0.5 and up is a triangle.

**multiply_mask** — `a * b` clamped to 0..1. `a`, `b` required. Out `mask`. The AND of two masks.

**scale_mask** — `mask * amount` clamped. `mask` required, `amount: proportion = 1`. Out `mask`.

**uniform_mask** — one global value as a mask. `coverage: proportion = 1`. Out `mask`.

**invert** — reflect a signal about the middle of its own clip-wide range. `value`, `samples = 1024` fixed. Out `value`.

**normalize** — map a signal to 0..1 by its own min and max over the clip's time. `value`, `samples = 1024`. Out `value: proportion`. A flat signal becomes 0.

**normalize_field** — map a signal to 0..1 by its min and max across the heads at each instant. `value` required. Out `value`. Not clamped.

**remap_field** — `(value - low) / (high - low)`, not clamped. `value`, `low = 0`, `high = 1`. Out `value`.

**profile_mask** — a band of `width` where an offset field crosses zero. `offset` required, `width: number = 0.25`, `shape = soft edges`. Out `mask`. Like `pill` for any numeric field, for example `radial_distance`.

**noise_mask** — animated coherent noise over mapped U and V. `period: beats = 4` (evolution), `scale: number = 2` (density), `shape = linear`. Out `mask`.

**pulse** — retrigger a brightness curve on each event. `trigger: events`, `duration: beats = 2`, `shape = fade out`. Out `mask`.

**event_envelope** — shape each event's own progress, max across overlapping events. `progress`, `weight = 1`, `shape = fade out`. Out `mask`.

**dissolve** — on each trigger, a random share of heads fades by a coverage curve. `trigger`, `duration: beats = 2`, `proportion: envelope = fade out`, `shape = flat`, `softness: proportion = 0`. Out `mask`.

**shimmer** — on each trigger, a fresh random `proportion` of heads with their own lifetimes. `trigger`, `duration: beats = 0.5`, `proportion = 0.25`, `shape = attack`. Out `mask`.

**random_heads_mask** — a rolling random subset that changes on a beat grid. `count: number = 1`, `repeat: beats = 1`, `delay: beats = 0`, `grid_aligned: bool = false`, `shuffle: bool = false`. Out `mask`. Shuffle off walks one seeded order; on draws a new set each change.

**random_selection** — a seeded random share of heads for a given index. `index: number = 0`, `proportion = 0.25`, `softness = 0`. Out `selected`. A new index is a new draw.

## Color

Colors are RGB triples in 0..1 or `#RRGGBB`. A gradient is `{"stops": [{"t": 0..1, "color": [r, g, b]}, ...]}`.

**gradient** — sample a gradient by clip progress. `gradient`. Out `color`.

**sample_gradient** — sample a gradient at a position. `gradient`, `position: proportion = 0`. Out `color`, `opacity`. A per-head field works as `position`.

**sample_field_gradient** — same with an explicit per-head `position` field.

**hsv** — HSV to RGB. `hue` (turns, wraps), `saturation = 1`, `value = 1`. Out `color`.

**rotate_hue** — rotate a color's hue. `color` required, `turns = 0`. Out `color`.

**mix_palette** — blend gradient stops by weights. `gradient`, `weights` (one channel per stop, required), `perceptual: bool = false`, `vibrance = 0.6`. Out `color`, `opacity`.

**palette_fallback** — use `fallback` when `gradient` has no stops. Out `gradient`.

**rainbow** — hue cycles on a beat grid. `repeat: beats = 4`, `delay = 0`, `grid_aligned = false`, `saturation = 1`. Out `color` at full value.

**wash** — the color unchanged. `color = white`. Out `color`.

**noise_wash** — `noise_mask` times a color. Adds `color`. Out `color`.

**random_heads** — `random_heads_mask` times a color. Adds `color`. Out `color`.

**strobe** — color through, plus a strobe rate. `color`, `rate: proportion = 0.9`. Out `color`, `strobe`. Wire both into `output`.

**beat_chase / beat_pulse / beat_shimmer / beat_dissolve** — `beat_trigger` into the named mask, times `color`. Inputs are the mask's inputs plus `color` and the beat clock's `repeat`, `delay`, `grid_aligned`. Out `color`. Use the mask node directly when the trigger is not the beat grid.

## Output

**output** — the one terminal. `color = white`, `pan: degrees = 0`, `tilt: degrees = 0`, `strobe: proportion = 0`, `speed: proportion = 1`. Out `lighting`.
Only wired ports are written. An unwired port leaves that capability alone,
which is not the same as writing black. The peak RGB channel becomes dimmer.
Wiring pan or tilt writes the whole position. Every playable graph needs one
`output` node and `graph.output(final.output("lighting"))`.

**write_position** — labelled passthrough. `pan`, `tilt` in degrees. Out `pan`, `tilt`.

**write_speed** — labelled passthrough. `value: proportion = 1`. Out `speed`.

**write_strobe** — coverage to a clamped strobe rate. `value = 1`. Out `strobe`.

## Math (core/*)

Binary nodes take `a`, `b` (required, broadcast) and return `value`:
**core/add**, **core/subtract**, **core/multiply**, **core/divide** (zero divisor gives 0), **core/minimum**, **core/maximum**.

Unary nodes take `value` and return `value`:
**core/absolute**, **core/floor**, **core/float32**, **core/fraction** (mod 1, never negative), **core/sine** (input in turns), **core/square_root** (errors on negatives).

**core/power** — `base ^ exponent`. `base` required, `exponent = 2`. Errors on non-real results.

**core/greater** — `a - b > tolerance` as 1 or 0. `a`, `b` required, `tolerance = 0`. Out `mask`. The standard threshold.

**core/choose** — per element, `yes` where `condition > 0` else `no`. All required. Out `value`.

**core/choose_number** — pick a whole branch by a boolean `condition`. `yes`, `no`. Out `value`.

**core/clamp_coverage** — clamp to 0..1. `value`. Out `mask`.

Channel axis: **core/channel** (`value`, `index = 0` fixed, one channel out), **core/channel_count**, **core/channel_index**, **core/channel_maximum**, **core/channel_sum**, **core/channel_argmax** (ties pick the lowest), **core/join_channels** (`a`, `b` concatenated).
Use `core/channel_maximum` to collapse overlapping events into one mask.

Head axis: **core/field_maximum**, **core/field_minimum**, **core/field_mean** (reduce across heads, broadcast back), **core/head_count** (number of heads), **core/distinct_count** (exact distinct values), **core/rank** (integer rank 0..n-1 ascending by `value`, ties by id; divide by `core/head_count` for 0..1), **core/field_first** (`value` at the lowest `order`, broadcast).

Random: **core/random** (`epoch: number = 0`, a new integer is a new draw; out 0..1 per head, seeded by the clip), **core/noise** (`x`, `y`, `z` required; 0..1 coherent noise, seeded by the clip), **core/value_noise_1d** (`seed`, `position`, `octaves = 1`), **core/value_noise_3d** (`seed`, `x`, `y`, `z`, `octaves = 1`), **core/seed_stream** (`seed`, `stream`; out a new `seed`).

## Events and clocks

**clip_time** — the clip's clock. Out `elapsed: beats`, `progress: proportion`, `duration: beats`, `beat: number` (track beat).

**core/track_time** — seconds clock. Out `seconds`, `clip_start`, `clip_duration`, `bpm`.

**rhythm** — repeating beat clock. `delay: beats = 0`, `repeat: beats = 4` (must be > 0), `grid_aligned: bool = false`. Out `elapsed` (0..repeat), `cycle: number`.

**beat_trigger** — periodic events on a beat grid. Same inputs as `rhythm`. Out `trigger: events`.

**core/grid_events** — events from the track's detected grid. `subdivision = 1`, `offset = 0`, `downbeats: bool = false`. Out `events`. Follows tempo changes.

**core/event_ages** — for each event within `duration`, its age. `events`, `duration: beats = 2`. Out `elapsed`, `progress`, `present`, `weight`, `index`, one channel per event. The base of every trigger-driven mask.

**core/event_window** — the `count` most recent events at `time`. `events`, `time: seconds = 0`, `count = 1`. Out `times`, `present`, `weights`, `index`.

**core/event_spacing** — minimum gap between events above `minimum` seconds. Out `spacing`.

**core/thin_events** — drop events closer than `minimum` seconds. Out `events`.

**random_subset** — each event addresses a random `proportion` of heads. `events`, `proportion = 0.25`, `seed`, `cycle: bool = false`. Out `events`.

**random_head_events** — per event, a random share of heads with progress and weight. `trigger`, `duration: beats = 0.5`, `proportion = 0.25`. Out `progress`, `weight`.

**clip_range** — min and max of a signal sampled once across the clip. `value`, `samples = 1024`. Out `minimum`, `maximum`.

## Common wirings

- Level meter: `band_mask` > `mapped_position` through `core/greater`, times `spatial_gradient`, into `output.color`.
- Beat chase: `beat_trigger` into `chase` with `resolve_mapping`, times a color.
- Drum hit: `drum_trigger` into `pulse`, times a color.
- Slow color field: `noise_mask` times `spatial_gradient`.
- Random sparkle: `beat_trigger` into `shimmer`, times a color.
- Any mask to a color: `core/multiply` with `a = color`, `b = mask`.
