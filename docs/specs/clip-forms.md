# Clip forms

Status: draft for sign-off. Scope: color and strobe. Aim (pan/tilt) is a later
spec. This spec replaces the authoring surface of
[composable-patterns.md](./composable-patterns.md). The graph engine stays as
the way forms are built; clips no longer hold graphs.

## Goal

A clip must be quick to author and obvious to read, for a person and for a
model that learns from saved scores. Today every effect is its own graph, and
all 1730 clips in the reference database point at score-local copies. After
this change, a clip names one **form** and sets its inputs. Named looks
(Chase, Pulse, Rainbow…) are **presets**: saved input values of a form.

Rules:

- One way to do each thing. If two inputs can make the same result, one goes.
- A form exists only if no other form can make its result, and it has a
  one-sentence meaning.
- A form shows at most about 7 inputs. Rare controls go under Advanced.
- Stored data is the form id, its version and every input value. It is the
  training label, so ids never change meaning. A change of meaning is a new
  version.
- No compatibility paths. Old data is converted once; the code keeps no second
  way to read it.

## Clip

A clip keeps its row: timing, selection, seed, z-index, blend mode, inputs.

- `graph` names a form with a version, for example `color.chase@1`. It never
  names a score-local graph. Score-local definitions are removed.
- `inputs` holds **every** input of the form. A clip is always created from a
  preset, and a preset sets all inputs, so no input is ever missing. A clip
  with a missing or unknown input is invalid.
- Every clip has `alpha` (proportion). Alpha is how much the clip counts. For
  light it multiplies the clip's output brightness; the compositor uses the top
  clip's brightness as its opacity, so alpha is also the clip's opacity. Alpha
  replaces clip fades. For aim (later spec) it is the blend toward this clip's
  aim, so a move is `alpha = time[0 → 1]`.

Layers combine forms. A "rainbow that chases" is a color layer with a chase
layer on top in `multiply`. There is no multiplying of forms inside one value.

## Picker

Right-click and the picker search **presets only**: the shipped presets.
Placing a preset copies its values into the clip. User-saved presets are out of
scope. The picker no longer lists
clips or graphs from other scores, and has no Built-in / Library / This score
split.

## Input values

An input takes a plain value of its type, or one of these **sources**. Each
form table below says which sources each input accepts ("promotable"). A
source is one level deep: a source's own settings are plain values.

| Source | Meaning |
|---|---|
| plain | One value, all heads, all the time |
| `time[...]` | One curve over the whole clip, all heads equal |
| `hit[...]` | One curve over the life of each event (chase, sparkle, and the hits of `color.constant`) |
| `noise(speed, range)` | Smooth random wandering over time |
| `audio(from_hz, to_hz, floor, threshold)` | Energy of one frequency range of the track's mix |

- `time[...]` and `hit[...]` are keyframes over progress 0–1. Segments are
  `hold`, `linear`, `step` or `bezier`. A `bezier` segment stores its two
  handles as `[progress, value]` in the curve's own units, like an envelope's
  Bézier, and plays exactly as drawn:
  `{"bezier": {"control1": [0.2, 0.9], "control2": [0.4, 0.1]}}`. A smooth
  ease is a Bézier with its handles a third of the way along, at the end
  values. Common curves are presets: ramp up, ramp
  down, swell, fade in, fade out, hold then drop.
- `audio` reads the full mix only. No stems, no drum events, no harmony.
  `from_hz` and `to_hz` set the frequency range (20–20,000 Hz, from < to).
  Named ranges only fill the two numbers: Kick 40–100, Bass 20–250, Mids
  250–4000, Highs 4000–16000, Full 20–16000. The energy is scaled over the
  clip, so its quietest moment is 0 and its loudest is 1. `floor` (0–1) is
  the lowest the value goes: value = floor + (1 − floor) × energy.
  `threshold` (0–1, default 0 = no gate) is a gate on the energy: energy
  below it gives 0, below the floor too; at or above it the value is
  floor + (1 − floor) × energy, not remapped from the threshold. The
  energy is the mean of the FFT magnitude bins in the range (2048-point FFT,
  about 21.5 Hz per bin at 44.1 kHz), so a narrow low range uses few bins.
  Stored form: `{"type": "audio", "value": {"from_hz": 40, "to_hz": 100,
  "floor": 0.3, "threshold": 0.5}}`. A threshold of 0 is left out.
- Engine note for speed inputs (`every`, `travel`) with a `time` curve: count
  strokes by adding up progress frame by frame, like an odometer, not by
  dividing the clock by the current speed. Dividing makes strokes jump when the
  speed changes. The sum must be computed from the curve, so seeking to a beat
  gives the same frame as playing to it.

Stored form of a source: a tagged value, for example
`{"type": "time", "value": {"points": [[0, 2], [1, 0.5]], "segments": ["linear"]}}`.
The engine turns each source into graph nodes when it prepares the clip.

## Forms

Promotable key: **T** time, **H** hit, **N** noise, **A** audio, **—** plain
only.

### `color.constant@1`

All selected heads one color, at one brightness.

| Input | Type | Promotable | Meaning |
|---|---|---|---|
| color | color | T | |
| brightness | proportion | T H N A | Multiplies the color |
| every | beats, or none | T | Time between hits |
| alpha | proportion | T N A | |

- **brightness** darkens this clip's light. **alpha** is how much the clip
  covers the layers under it. A clip at brightness 50% and alpha 100% is a
  dim light that hides what is under it; at brightness 100% and alpha 50% it
  is a full light at half opacity.
- **every** has the meaning of `every` on `color.time`: 0 (none, the
  default) is one hit over the whole clip, N is a hit every N beats. Only
  `brightness = hit[...]` reads the hits; each hit lives until the next one.
  With a plain brightness, `every` has no effect.

Presets: Wash (brightness 100%, every none), Pulse (brightness
`hit[hold then drop]`, every 1b).

### `color.time@1`

A color gradient over time, all heads equal.

| Input | Type | Promotable |
|---|---|---|
| colors | gradient | — |
| curve | curve preset or curve | — |
| every | beats, or none | T |
| alpha | proportion | T N A |

With `every` = none, the gradient plays once over the clip. With `every` = N
beats, it repeats every N beats. `every` means "repeat period" in every form,
and 0 beats is once over the clip in every form. On chase and sparkle, a
`travel` or `duration` of 0 beats is the whole clip.
Presets: Color fade (none), Rainbow (hue gradient, every 4b).

### `color.space@1`

A gradient laid across the rig. Each head gets a fixed color from its
position. It does not move.

| Input | Type | Promotable |
|---|---|---|
| colors | gradient | — |
| axis | axis | — |
| alpha | proportion | T N A |

Preset: Gradient.

### `color.chase@1`

Strokes travel across the heads. Each event starts one stroke.

| Input | Type | Promotable | Meaning |
|---|---|---|---|
| color | color | T | Stroke color |
| axis | axis | — | Which way the heads are ordered |
| every | beats, or event list | T | Time between strokes |
| travel | beats | T | Time for one stroke to cross the whole axis |
| width | number 0–4 + rel/abs | T H | Stroke size |
| shape | shape preset or curve | — | Brightness across the stroke |
| path | path preset or curve | — | Where the stroke is over its life |
| alpha | proportion | T H N A | |
| Advanced: boundary | clip / wrap | — | What happens at the ends |

- **axis** is `order`, `x`, `y`, `z`, `radial`, `angle` or a custom
  `vector(u, v, z)`, with an optional `mirror`. Axis has no `reverse`; path
  owns direction. Every axis (chase, `color.space`) also has:
  - **Spans**: `selection` (default: one axis 0–1 across the whole
    selection), `fixture` (each fixture, the head id before its last `:`,
    gets its own axis 0–1, so a chase runs along every pixel bar or LED ring
    at once) or `group` (each group of the selection expression gets its own
    axis, so `left_truss | right_truss` chases along both trusses in
    parallel). Path, mirror, width and shape apply within each span. Stored
    as `"span": "fixture"`; `selection` is left out. The old `per_group` flag
    is the group span; a form axis stores `per_group: false`.
  - **Plane** (radial and angle only, required for them): `Auto` (default),
    `Around up–down`, `Around front–back`, `Around left–right` or
    `Custom axis` (a U, V, Z normal, like mirror's custom plane). Stored as
    `"plane": {"kind": "auto"}`, `up_down`, `front_back`, `left_right` or
    `{"kind": "custom", "normal": [u, v, z]}`. Angle 0 and its direction:
    around up–down, from stage right (U+) toward downstage (V+); around
    front–back, from stage right toward up (Z+); around left–right, from
    downstage toward up. A custom normal takes angle 0 from the fixed plane
    whose axis it is closest to (ties: up–down, front–back, left–right),
    projected onto its plane, and turns by the right-hand rule around the
    normal as given.
  - **Auto rule.** The normal is the direction of least spread of the heads
    in the span (the plane holds their two largest spread directions). Heads
    on one line or one spot use around up–down. The normal then takes its
    sign and its angle 0 from the fixed plane it is closest to, as above. So
    a flat floor ring reads exactly like around up–down, a ring facing the
    audience exactly like around front–back, and a slightly tilted rig keeps
    the same angle 0 and direction. A rig near 45° between two fixed planes
    can change reading when it tilts across the middle.
  - **Center.** Radial and angle measure from the centroid (mean head
    position) of the span, in its plane. With the fixture span, each
    fixture turns around its own center. Radial is the distance within the
    plane, scaled 0–1 over the span.
- **every** is beats, or a list of stamped event times in beats from the clip
  start. No `delay` and no `grid_aligned`: to shift, move the clip.
- **Strokes on the axis at once** = `travel / every`. Strokes overlap when
  events come faster than travel.
- **Overrun.** With boundary `clip` and a gliding path, a stroke enters fully
  from outside the axis and leaves fully: path progress 0–1 maps onto stroke
  centers from `−w/2` to `1 + w/2`, where `w` is the absolute width. Travel is
  the time from first light to fully gone. A stroke is dark at the start and
  at the end of its life. Stepped paths (`steps(N)`, or any path with `hold`
  or `step` segments) keep their exact positions, and `wrap` has no overrun.
- **width** `abs` is a share of the axis, from 0 to 4. Above 1 a stroke is
  wider than the axis: a soft wide stroke keeps part of the rig lit through
  its whole life. `rel` is relative to the gap between strokes. Let
  `g = every / travel` (per stroke, when `every` or `travel` change). With
  overrun, stroke centers are `(1 + w) × g` apart, so strokes just touch at
  rel 100% when `w = g / (1 − g)`. Absolute width is `r g / (1 − r g)`, with
  `r g` capped at 4/5: at most a stroke four axes wide. When `every ≥ travel`
  a stroke has left before the next one enters, so rel 100% gives that widest
  stroke. Without overrun, absolute width is `r g`, capped at 4.
- In storage, `width` holds the share and a separate boolean input
  `width_relative` holds rel (true) or abs (false).
- **shape** presets: hard, soft, comet, reverse comet, spike. No preset has
  more than one bump; more strokes come from `every`. An asymmetric shape
  follows the travel direction, so a comet tail trails when the path runs
  backward.
- **path** presets: forward, backward, bounce, ease in, ease out, ease in-out,
  `steps(N)`. A custom path is a curve of position (0–1) over the stroke's
  life; a partial range replaces start/end inputs. `steps(N)` jumps between N
  positions at the centers of N equal parts, `(i + 0.5) / N`, and does not
  glide.

The default axis is `x`.

Presets: Chase, Wave (soft, width abs 100%), Ripple (radial, plane Auto),
Spin (angle, plane Auto), Bounce (bounce),
Alternating sides (x, `steps(2)`, travel = every, width abs 50%, hard),
Stepped chase (`steps(N)`, width abs 1/N),
Grow (radial, fixture span, plane Auto, width abs 125%, hard, path 0 → 0.5,
every and travel 0: one stroke over the clip).

Grow lights each fixture from its middle out to both ends, and a lit head
stays lit. On a straight bar, radial 0 is the middle head and 1 are both end
heads. With overrun, the lit part is radial `[0, p × (1 + w) / 2)` at clip
progress `p`, so with width 1 the end heads would light only at the last
instant of the clip. Width 125% reaches both ends 8/9 of the way through the
clip and holds all heads lit to its end.

### `color.sparkle@1`

Each event lights a random share of the heads.

| Input | Type | Promotable | Meaning |
|---|---|---|---|
| color | color | T | |
| every | beats, or event list | T | Time between events |
| duration | beats | T | Life of one event |
| coverage | proportion | T H N A | Share of heads lit, below 100% |
| brightness | proportion | T H N A | Brightness of the lit heads |
| grain | head / fixture / clump(N) | — | What one "head" is |
| alpha | proportion | T N A | |

- A new random order is drawn per event from the clip seed and stable head
  identity. It never depends on frame count.
- Events overlap when `every < duration`; overlaps keep the maximum.
- Sparkle is random heads only. A fixed coverage of 100% lights every head,
  which is a Wash with a brightness per hit, so validation rejects it. A
  sparkle with stamped events may still have 100%: the Wash has no stamped
  hits. A curve may pass through 100% (Dissolve, Build).

Presets:

| Preset | coverage | brightness |
|---|---|---|
| Dissolve | `hit[100% → 0%]` | 100% |
| Build | `hit[0% → 100%]` | 100% |
| Random heads | 50% | 100% |
| Shimmer | 30%, every 1/8b | `hit[spike]` |

### `color.noise@1`

Soft brightness that wanders across space and time.

| Input | Type | Promotable |
|---|---|---|
| color | color | T |
| speed | beats | T |
| scale | proportion (blob size, share of the rig) | T |
| contrast | proportion | T N A |
| alpha | proportion | T N A |

Presets: Drift, Atmosphere, Aurora.

### `strobe.constant@1`

Fixture shutter strobe. The form writes only the strobe channel, never color
or dimmer, so it strobes whatever the layers under it light. The written rate
is `rate × alpha`; at alpha 0 the strobe stops. A colored strobe is two
layers: a color form, and a strobe layer above it.

| Input | Type | Promotable |
|---|---|---|
| rate | proportion | T N A |
| alpha | proportion | T N A |

## Removed

- Score-local definitions and the score graph tab for clips. The graph editor
  remains for building shipped forms.
- Recipes that the forms replace: wash, gradient, spatial_gradient, rainbow,
  chase, beat_chase, pulse, beat_pulse, dissolve, beat_dissolve,
  beat_shimmer, random_heads, random_heads_mask, noise_wash, pill, band_pulse,
  drum_pulse, strobe. `write_strobe` stays as a building block.
- `drum_trigger`, `drum_mask`, `harmony_color` and the harmony source.
- Stem selection on audio sources.
- `delay`, `grid_aligned` and mapping `reverse` everywhere.

## Migration

Every clip converts. A clip that no rule matches is a bug in the rules, not a
case to keep; the migration does not finish until the report is empty.

| Definition today (clips) | Becomes |
|---|---|
| Color fade (340) | `color.time`, gradient and curve copied |
| Pulse, Beat pulse, one-shot, drum bloom (~370) | `color.sparkle`, Pulse |
| Chase, Beat chase, tide, Mirrored chase, Circle chase, Outward pulse, radial bloom (~290) | `color.chase` |
| Wash, wash (141) | `color.constant` |
| Alternating sides (20), Alternating colors, Stepped circle chase | `color.chase` with `steps(N)` |
| Random heads, Dissolve variants, shimmer (~40) | `color.sparkle` |
| Noise wash, aurora, Atmosphere (~50) | `color.noise` |
| Rainbow (13) | `color.time`, Rainbow |
| Strobe output, strobe burst (~73) | `strobe.constant` |
| A colored strobe (Bass strobe) | a color layer (the color product, as for a color clip) under `strobe.constant` |
| A form × a curve over the clip (Tidal wave, Bounce, Gather, Bloom, drop1…, ~110) | the form, with the curve moved to `alpha = time[...]` |
| Bass follow, Bass pulse, Bass strobe (~165) | the form, with `alpha` = `audio(low, ...)`; for Bass strobe on the color layer |

Conversions of removed inputs:

- `delay` and `grid_aligned`: move the clip start to the first event and
  shorten it by the same amount. This is exact when nothing is lit before the
  first event; the render check proves it per clip.
- Drum triggers: stamp the analyzed hit times into `every` as an event list.
  The output is unchanged and the score no longer depends on drum analysis.
- Harmony color (3): bake into a `color.time` curve.
- Mapping `reverse`: `path = backward` on chase; reversed stops on a
  gradient.
- Stem audio: the matching band of the mix. Output changes; the report lists
  these clips.

**Wash brightness (2026-09-24).** `color.constant@1` gained `brightness`
and `every` in place, and Pulse moved from sparkle to the Wash. Stored Wash
clips got `brightness = 1` and `every = 0`, which renders the same
(`color × 1 × alpha`). Every sparkle with a fixed coverage of 100% and paced
events became a Wash with the same color, brightness, every and alpha. At
coverage 100% the grain has no effect. When `duration` equals `every` the
hit curve is kept; when `duration < every` the curve is squeezed into the
first `duration / every` of each hit and the rest is 0, as the sparkle was
dark between events. Sparkles with stamped events stay sparkles.

**Render check.** A dry run over a copy of the reference database renders
every converted clip, old and new, at a fixed set of times on its real venue,
and reports the largest difference per clip. A conversion counts as exact below
2e-6 per channel. Every non-exact clip is listed with its reason.

**Where it runs.** Preferred: a one-time job that rewrites the rows on the
server, which then sync down to every device. The app then contains no
migration code and reads only the new format. This needs an investigation
before slice 1 (see below). Fallback: one pass on the local database at
startup, deleted in the release after.

### Investigation: server-side migration

Answer before building the migration:

- Can a one-time job (a Rust binary with the service role, or SQL) rewrite
  `clips` and `score_definitions` in Supabase, and do the changed rows reach
  every device through PowerSync as normal downloads?
- What happens to a device with unsent local changes in the old format, or an
  old app build that writes the old format after the job? Is an app-version
  gate needed (refuse to sync below a minimum build)?
- The render check needs the analyzed track data and venues. Does the job run
  where that data exists, or is the render check a separate local dry run
  before the job?
- The existing load-time repair (`migration::lit_heads_density`) is the same
  kind of runtime code. Remove it the same way.

## Python and the in-app agent

`edit.add_clip(preset="Alternating sides", beats=(32, 48), inputs={"every":
{"time": [[0, 2], [1, 0.5]]}})`. A clip always starts from a preset; `inputs`
overrides some of its values. The node-cards skill is rewritten around forms
and presets. A text format for training data is a later spec; it must map one
to one onto the stored form.

## Delivery

Slice 1 is Chase end to end: engine additions (curve hold/step, `alpha`,
sources, stamped events, relative width, `steps(N)`, direction-following
shape, custom vector axis), `color.chase@1`, presets, the inspector with
promotion, and migration with the render check for the chase rows of the
table. Slice 2 adds the other forms and the preset-only picker. Slice 3
updates the Python API, skills, docs and removes the old recipes and
score-local definitions.

## Acceptance (slice 1)

- Place Chase from the picker; set every input in the inspector; promote
  `every` to a time curve and see strokes multiply without a jump.
- Alternating sides renders identically to its old graph.
- A drum-triggered chase migrates to stamped events with no render difference.
- Seek to any beat and get the same frame as playback.
- Patterns crate tests and the affected headless tests pass.

## Open questions

1. Sparkle today has a no-repeat order (Random heads with shuffle off: a head
   does not light again until all others have). Keep it as an option, or
   always draw a fresh random set?
2. `color.time` has an optional `every` (for Rainbow). Confirm.
