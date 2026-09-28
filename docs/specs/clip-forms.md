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

- `graph` names a form with a version, for example `color@1`. It never
  names a score-local graph. Score-local definitions are removed.
- `inputs` holds **every** input of the form. A clip is always created from a
  preset, and a preset sets all inputs, so no input is ever missing. A clip
  with a missing or unknown input is invalid.
- Every clip has `alpha` (proportion). Alpha is how much the clip counts. For
  light it multiplies the clip's output brightness; the compositor uses the top
  clip's brightness as its opacity, so alpha is also the clip's opacity. Alpha
  replaces clip fades. For aim (later spec) it is the blend toward this clip's
  aim, so a move is `alpha = time[0 → 1]`.

Layers combine forms. Inside one `color@1` clip, color and brightness each
follow their own source, so a "rainbow that chases" is one clip: a color
gradient over time and a moving space source on brightness. There is no
multiplying of forms inside one value.

**Blending.** A layer blends onto the light under it: color × dimmer, per
head. No clip on a head is no light. A head that a lower clip left dark is
also no light, and its color has no effect. So every blend mode is its math
on that light, with no special case for the first layer:

- `replace` sets the light. Black on replace paints black. Over no light it
  gives the top light.
- `add`, `screen`, `max` and `lighten` over black give the same result as
  over nothing: the top light.
- `multiply`, `min` and `subtract` over nothing give nothing. A mask over no
  light is no light.
- Strobe blends the same way, from 0 where nothing is under it.

Alpha multiplies the clip's brightness. So a clip at alpha 0 adds no light:
under any layer, and on top in `add`, `screen`, `max`, `lighten` or
`subtract`, it is the same as no clip. On top in `replace`, `multiply` or
`min` it is black, the same as brightness 0.

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
| `hit[...]` | One curve over the life of each hit (each `every` period or stroke of `color@1`, each sparkle event) |
| `noise(speed, range)` | Smooth random wandering over time |
| `audio(from_hz, to_hz, floor, threshold)` | Energy of one frequency range of the track's mix |
| `space(axis, values, move)` | Values along an axis of the heads; with `move`, a stroke that travels along it |

- `time[...]` and `hit[...]` are keyframes over progress 0–1, in the one
  curve format that envelopes also use: points `[x, value]` or
  `[x, value, ease]`, x strictly increasing from 0 to 1. The ease moves the
  value from its point to the next: `linear` (the default, not written),
  `ease-in`, `ease-out`, `ease-in-out`, `hold` (jump at the next point), or
  a drawn `[x1, y1, x2, y2]`, a CSS cubic-bezier local to the segment that
  plays exactly as drawn. The last point has no ease:
  `{"points": [[0, 0, "ease-in"], [0.5, 1, "hold"], [0.8, 1], [1, 0]]}`.
  Common curves are presets: ramp up, ramp down, swell, fade in, fade out,
  hold then drop.
- On a color input, `time[...]` and `hit[...]` can read a gradient instead
  of color keys: `{"gradient": {"stops": [...]}, "curve": {"points": [[0,
  0], [1, 1]]}}`. The curve gives the gradient position (0–1) at each
  progress. The gradient blends in OKLab; color keys blend in RGB. Every
  color is linear Rec. 2020, 0–1 per channel.
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
  about 23.4 Hz per bin at 48 kHz), so a narrow low range uses few bins.
  Stored form: `{"type": "audio", "value": {"from_hz": 40, "to_hz": 100,
  "floor": 0.3, "threshold": 0.5}}`. A threshold of 0 is left out.
- Engine note for speed inputs (`every`, `travel`) with a `time` curve: count
  strokes by adding up progress frame by frame, like an odometer, not by
  dividing the clock by the current speed. Dividing makes strokes jump when the
  speed changes. The sum must be computed from the curve, so seeking to a beat
  gives the same frame as playing to it.

Stored form of a source: a tagged value, for example
`{"type": "time", "value": {"points": [[0, 2, "ease-out"], [1, 0.5]]}}`.
The engine turns each source into graph nodes when it prepares the clip.

## Forms

Promotable key: **T** time, **H** hit, **N** noise, **A** audio, **S**
space, **—** plain only.

### `color@1`

One color on the selected heads. Each input is fixed, over time, per hit,
across space, or across space and time.

| Input | Type | Promotable | Meaning |
|---|---|---|---|
| color | color | T H S | |
| brightness | proportion | T H N A S | Multiplies the color |
| every | beats | T | Time between hits |
| alpha | proportion | T H N A | |

- **brightness** darkens this clip's light. **alpha** is how much the clip
  covers the layers under it. A clip at brightness 50% and alpha 100% is a
  dim light that hides what is under it; at brightness 100% and alpha 50% it
  is a full light at half opacity.
- **every**: 0 (the default) is one hit over the whole clip, N is a hit every
  N beats. A `hit[...]` source plays once per hit; each hit lives until the
  next one. A moving space source sends one stroke per hit. With no hit
  source and no moving space source, `every` has no effect.
- A color gradient per hit is a color fade; with `every` N it repeats every N
  beats (Rainbow).

#### Space source

Stored form:

```json
{"type": "space", "value": {
  "axis": {"source": {"kind": "u"}, "per_group": false, "reverse": false},
  "gradient": {"stops": [...]}}}
```

on a color, or `"curve": {"points": [...]}` (values 0–1) in place of
`"gradient"` on brightness. Each head reads the gradient or the curve at its
position on the axis. A still space source does not change over time.

- **axis** is `order`, `x`, `y`, `z`, `radial`, `angle`, `random` or a
  custom `vector(u, v, z)`, with an optional `mirror`. Axis has no
  `reverse` and no `per_group`; a backward path owns direction and the group
  span owns grouping. Every axis (a space source, aim) also has:
  - **Random**: each head's coordinate comes from the clip's seed and the
    head id. Within each span the heads take the evenly spaced values
    `i / (n − 1)` (one head: 0.5) in a shuffled order, not independent
    random values, so path, width and an aim's spread still cover the whole
    range evenly. The order is the same on every frame and after a seek; a
    new seed gives a new order. Stored as `"source": {"kind": "random"}`.
  - **Mirror** reflects the heads on the low side of a plane onto the
    high side before the axis reads them, so both halves of a span do the
    same. The sheet's Mirror row is the old mapping editor's control:
    `Off`, `Left–right`, `Front–back`, `Up–down` (planes through the middle
    of the span, with normal U, V or Z) and `Custom plane`, which shows the
    plane's normal as U, V, Z fields. With a mirror on, an `Offset` field
    moves the plane along its normal, in metres. A stored normal that is
    none of the three fixed ones reads `Custom plane`. A mirror needs a
    spatial axis along a line: `x`, `y`, `z` or `vector`. Order, radial,
    angle and random take no mirror; the sheet hides the row for them, and
    picking one of them drops a mirror. Stored as
    `"mirror": {"normal": [1, 0, 0], "offset": 0}`. For color forms the
    folded coordinate is the whole mirror. An aim also mirrors its fan and
    motion; see [aim.md](./aim.md#mirror).
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

#### Moving space source: a chase

A space source on brightness can move. Add `"move"`:

```json
"move": {"path": {"points": [[0, 0], [1, 1]]},
         "travel": {"type": "beats", "value": 2},
         "width": {"type": "number", "value": 0.2},
         "width_relative": true, "boundary": "clip"}
```

Then the curve is one stroke: its brightness from the tail (0) to the head
(1) in the direction of travel. Heads outside a stroke get 0. Each hit of
`every` starts one stroke. `travel` is beats or a `time` curve of beats;
`width` is a number 0–4 or a `time` or `hit` curve; `boundary` is `clip` or
`wrap`. A color does not move: outside a stroke it would have no light.

- With a moving brightness, `hit[...]` sources on the other inputs follow
  each stroke's life, one channel per live stroke; where strokes overlap the
  brightest wins. A color cannot take a hit source then; use a time source.
  A clip has one moving space source at most.
- **every** is beats. No `delay` and no `grid_aligned`: to shift, move the
  clip. A hit is a clip that you place on the timeline. One clip never holds
  a list of hit times: hits that do not repeat at a fixed period are
  separate clips with `every` = 0.
- **Strokes on the axis at once** = `travel / every`. Strokes overlap when
  events come faster than travel.
- **Overrun.** With boundary `clip` and a gliding path, a stroke enters fully
  from outside the axis and leaves fully: path progress 0–1 maps onto stroke
  centers from `−w/2` to `1 + w/2`, where `w` is the absolute width. Travel is
  the time from first light to fully gone. A stroke is dark at the start and
  at the end of its life. Stepped paths (`steps(N)`, or any path with a
  `hold` ease) keep their exact positions, and `wrap` has no overrun.
- **width** `abs` is a share of the axis, from 0 to 4. Above 1 a stroke is
  wider than the axis: a soft wide stroke keeps part of the rig lit through
  its whole life. `rel` is relative to the gap between strokes. Let
  `g = every / travel` (per stroke, when `every` or `travel` change). With
  overrun, stroke centers are `(1 + w) × g` apart, so strokes just touch at
  rel 100% when `w = g / (1 − g)`. Absolute width is `r g / (1 − r g)`, with
  `r g` capped at 4/5: at most a stroke four axes wide. When `every ≥ travel`
  a stroke has left before the next one enters, so rel 100% gives that widest
  stroke. Without overrun, absolute width is `r g`, capped at 4.
- In storage, `width` holds the share and the boolean `width_relative`
  holds rel (true) or abs (false).
- **shape** (the source's curve) presets: hard, soft, comet, reverse comet, spike. No preset has
  more than one bump; more strokes come from `every`. An asymmetric shape
  follows the travel direction, so a comet tail trails when the path runs
  backward.
- **path** presets: forward, backward, bounce, ease in, ease out, ease in-out,
  `steps(N)`. A custom path is a curve of position (0–1) over the stroke's
  life; a partial range replaces start/end inputs. `steps(N)` jumps between N
  positions at the centers of N equal parts, `(i + 0.5) / N`, and does not
  glide.

Presets:

| Preset | color | brightness | every |
|---|---|---|---|
| Wash | white | 100% | 0 |
| Pulse | white | `hit[hold then drop]` | 1b |
| Color fade | hit gradient, linear | 100% | 0 |
| Rainbow | hit hue gradient | 100% | 4b |
| Gradient | space gradient along `x` | 100% | 0 |
| Chase | white | moving, hard, width rel 20% | 2b |

Moving-brightness presets also include Wave (soft, width abs 100%), Ripple
(radial, plane Auto), Spin (angle, plane Auto, comet, wrap), Bounce (bounce),
Alternating sides (x, `steps(2)`, travel = every, width abs 50%, hard),
Stepped chase (`steps(N)`, width abs 1/N), and
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
| every | beats | T | Time between events |
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
  curve may pass through 100% (Dissolve, Build).

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
| Color fade (340) | `color@1`, color a hit gradient: gradient and curve copied |
| Pulse, Beat pulse, one-shot, drum bloom (~370) | `color.sparkle`, Pulse |
| Chase, Beat chase, tide, Mirrored chase, Circle chase, Outward pulse, radial bloom (~290) | `color@1`, brightness a moving space source |
| Wash, wash (141) | `color@1` |
| Alternating sides (20), Alternating colors, Stepped circle chase | `color@1`, moving brightness with a `steps(N)` path |
| Random heads, Dissolve variants, shimmer (~40) | `color.sparkle` |
| Noise wash, aurora, Atmosphere (~50) | `color.noise` |
| Rainbow (13) | `color@1`, Rainbow |
| Strobe output, strobe burst (~73) | `strobe.constant` |
| A colored strobe (Bass strobe) | a color layer (the color product, as for a color clip) under `strobe.constant` |
| A form × a curve over the clip (Tidal wave, Bounce, Gather, Bloom, drop1…, ~110) | the form, with the curve moved to `alpha = time[...]` |
| Bass follow, Bass pulse, Bass strobe (~165) | the form, with `alpha` = `audio(low, ...)`; for Bass strobe on the color layer |

Conversions of removed inputs:

- `delay` and `grid_aligned`: move the clip start to the first event and
  shorten it by the same amount. This is exact when nothing is lit before the
  first event; the render check proves it per clip.
- Drum triggers: one clip per analyzed hit, placed at the hit. The score no
  longer depends on drum analysis.
- Harmony color (3): bake into a color curve.
- Mapping `reverse`: a backward path on a moving space source; reversed
  stops on a gradient.
- Stem audio: the matching band of the mix. Output changes; the report lists
  these clips.

**Wash brightness (2026-09-24).** The Wash form (now `color@1`) gained `brightness`
and `every` in place, and Pulse moved from sparkle to the Wash. Stored Wash
clips got `brightness = 1` and `every = 0`, which renders the same
(`color × 1 × alpha`). Every sparkle with a fixed coverage of 100% and paced
events became a Wash with the same color, brightness, every and alpha. At
coverage 100% the grain has no effect. When `duration` equals `every` the
hit curve is kept; when `duration < every` the curve is squeezed into the
first `duration / every` of each hit and the rest is 0, as the sparkle was
dark between events.

**Stamped events split (2026-09-24).** `every` no longer takes a list of
hit times. 66 stored clips had a list: 63 sparkles, all at coverage 100%,
and 3 chases, with 1012 hits. Each became one clip per hit, on the same
layer, with the same blend, selection, seed and alpha. The clips of one old
clip do not overlap in time and keep its place in the paint order.

- A sparkle became Washes with `every = 0`. The old clip showed the
  brightest live hit, so its span is split where the brightest hit changes.
  Each Wash plays the part of its hit's curve that it covers. A hit that is
  always under a brighter one gets no clip (1 hit).
- Where the old clip hid the layers under it between hits (`replace`,
  `multiply`, or a later layer that blends onto its dark heads), the Washes
  also cover the dark time: a dark tail to the next hit and a dark lead-in
  before the first hit. Otherwise a Wash covers only its hit's life, cut at
  the old clip end.
- A chase became one chase per hit with `every = 0` and the same travel.
  One `multiply` chase got a Wash at brightness 0 before its first hit, to
  keep the dark lead-in.
- The render check on the full score was exact for 64 of 66 clips. The two
  chases that differ have double hits about 0.02 beats apart. One clip took
  the brightest of two overlapping strokes per head; two clips on one layer
  paint in order instead. Largest difference: 0.28 (`multiply` chase) and
  0.049 (`replace` chase).

**Dark padding removed (2026-09-24).** No clip is black only, and a clip
does not run dark before or after the part where it lights. The split's
dark padding was removed from its clips: the dark lead-in Wash was deleted,
454 Washes were cut to where their brightness curve lights (8 at the start,
453 at the end), and 25 chases were cut to their travel. Seven chases that
started about 0.02 beats after the previous chase of the same old clip were
one hit detected twice; the later one was deleted. The look changed where
the padding hid the layers under it: that light now shows between hits.

**One color form (2026-09-27).** `color.constant@1`, `color.time@1`,
`color.space@1` and `color.chase@1` merged into `color@1`. Rows are not
migrated. `luma_patterns::upgrade` reads an old clip as `color@1` when it is
loaded, and the first save that changes the clip writes the new form. The
Wash form keeps its inputs. Color over time becomes a color hit gradient with
the same `every` and brightness 100%. Color across space becomes a color
space gradient with brightness 100% and `every` 0. A chase becomes a moving
space source on brightness: its axis, shape (the curve), path, travel, width,
relative width and boundary go into the source; color, every and alpha stay.
A recorded test shows the same light for every conversion.

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
sources, relative width, `steps(N)`, direction-following
shape, custom vector axis), the chase form (now `color@1` with a moving
brightness), presets, the inspector with
promotion, and migration with the render check for the chase rows of the
table. Slice 2 adds the other forms and the preset-only picker. Slice 3
updates the Python API, skills, docs and removes the old recipes and
score-local definitions.

## Acceptance (slice 1)

- Place Chase from the picker; set every input in the inspector; promote
  `every` to a time curve and see strokes multiply without a jump.
- Alternating sides renders identically to its old graph.
- A drum-triggered chase migrates to one chase clip per hit.
- Seek to any beat and get the same frame as playback.
- Patterns crate tests and the affected headless tests pass.

## Open questions

1. Sparkle today has a no-repeat order (Random heads with shuffle off: a head
   does not light again until all others have). Keep it as an option, or
   always draw a fresh random set?
2. Settled: `every` on `color@1` repeats a color hit gradient (Rainbow).
