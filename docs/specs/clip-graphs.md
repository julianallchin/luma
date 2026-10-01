# Clip graphs

Build spec. Branch `clip-graphs`, baseline `6ebfb8f4`. Julian approved the
direction on 2026-09-28. The calls he did not settle are in section 11.

Every clip owns one small graph. The inspector shows that graph. Agents write
it as Python. This spec replaces forms, sources, events-on-sources and the
form presets. It keeps the tensor runtime, the aim blending, the one curve
format and Rec. 2020 colors.

Write it in plain words: a value is typed, a choice is a setting, anything
that varies is a wire.

---

## 0. Graph version 3 (2026-09-30)

Julian's foundation decisions. This section wins over the sections below
where they differ; the sections below still hold for everything it does
not name.

Principles: each light = f(its field, time), on tensors [lights × time ×
events] that broadcast. Every number means what it says: positions are 0–1
along a space's direction within the clip's selection (per span after a
split), times are beats, an agent never needs the rig's size. Code ===
graph, names included. An LD thinks in three things: color (which color),
brightness (the pattern across lights: chase, pulse, cut), alpha (the whole
clip's opacity over time; the timeline fade points edit it).

Kinds (13): `time`, `space`, `noise`, `audio`, `curve`, `math`, `mirror`,
`shuffle`, `group`, `split`, `color`, `aim`, `strobe`. `clock` is gone.

| Change | v3 |
|---|---|
| alpha | Opacity. `color`: light = color × brightness; then the clip's light blends with the light below by its blend mode, and the result mixes with the light below by alpha: `out = below + (blend(below, clip) − below) × alpha`. `strobe` the same on the shutter (shutter = rate). `aim` unchanged (alpha is the aim's weight). Alpha 0 shows the clip below, in every blend mode. The overlap ranking (5.3) still multiplies all factors, alpha included. The lighting tensor's channel 11 is the clip's alpha for color and strobe (the aim weight for aim); `FixtureOutput.alpha`. |
| time | `time(every, duration, delay, phase)`. `every` beats > 0 (T, H): events start every `every` beats from the clip start; empty = once over the clip, no events. `duration` beats > 0 (T, H): each event's life; empty = `every`, or the clip's length when there is no `every` (with no `every` and a `duration`, one event of that length from the clip start). `delay` beats, any sign (T, H). `phase` turns, any (T, H). Per light: `p` = the event's age over its duration, 0–1 (clamped); `τ = p − delay / duration`; with a phase, `τ = (τ + phase) mod 1`. τ is not clamped: before a light's start τ < 0 and a curve holds its first value there (a waiting light of a dissolve stays on); after its end a curve holds its last value. Two time nodes with `every` whose `every` and `duration` inputs are equal (same numbers or the same wires) share one set of events (one clock). |
| shuffle | `shuffle(heads, time)`: `time` is a wire from a time node; with `every`, a new order per event; without, one order. |
| space | `space(heads, direction, shift, scale, kind, wrap)`; `scale` is the old `length` (share ≥ 0), same math: `x = (a − shift) / scale`, wrapped first when `wrap`. A `line` space whose direction is parallel to the normal of a mirror in its heads (the latest such mirror) measures from that mirror's plane: `a = (p − plane) · direction / R`, `R` the span's extent along the direction before any fold; 0 is on the plane. |
| mirror | `mirror(heads, normal, at)`. `at` share (0–1, T): the plane sits at `at` along the normal across the span's positions before any fold (the original selection), so 0.5 is always the centre, also for stacked mirrors. Empty 0.5. Heads on the low side reflect; aim yaw and pitch mirror. |
| math | `math`: setting `op` = `*` \| `+` \| `-` \| `max` \| `min` (default `*`); input `values`: a list of numbers and value wires (curves or math). `*`, `+`, `max`, `min` take 2 or more items, `-` exactly 2 (`a − b`). Tensors broadcast: a number times a color is a color. The output is a value of the widest input kind (color > vector > number; color with vector is an error). A math result wires like a curve, into any value input or a curve's low or high. Its inputs carry at most one clock. |
| lists | Gone from outputs. `brightness=[a, b]` is `brightness=a * b` (one math node). |
| curve | A node; low and high wireable. At a jump (two points at one x), x itself reads the larger value: a head exactly on an edge is lit. Ties are decided the same way for every jump, over space or time, inside 0–1 or at its ends. So Slash's cut lights the far corner when its front stops on it; a step in time is on at the beat it switches on or off; the middle rank of an odd count sits on a 0.5 jump and is lit. A `hold` ease is not a jump. |

Python: `time(every=None, duration=None, delay=None, phase=None)`,
`space(heads=None, direction=None, shift=None, scale=None, kind="line", wrap=None)`,
`mirror(heads=None, normal=None, at=None)`, `shuffle(heads=None, time=None)`.
Math is plain operators on value nodes: `a * b * c` is one math node with
three items (a chain of one operator flattens), `a - b`, `0.5 * a`,
`max(a, b)`, `min(a, b, c)`. A named math result (`glow = a * b`) is its own
node and links like any node. `source()` writes a curve or math node inline
as the argument it feeds (`shift=curve(t, "Ramp up")`, `brightness=cut *
bloom * fade`) when exactly one input reads it and its id is a numbered id
(`curve3`, `math1`); a named or shared one gets its own line. Code → graph →
code keeps nodes, names, inline curves and `a * b * c` exactly.

Migration (`backend/scripts/migrate_clip_graphs_v3.py`, on top of 8.7):
version 1 or 2 → 3. A clock node becomes the `every`/`duration` of each time
node that read it (shared clocks stay shared by equal inputs); a shuffle's
clock becomes a time node with that `every`/`duration`; `time.delay` turns →
beats (× the event duration, or × the clip's length with no clock);
`space.length` → `scale`; a list → one `*` math node; an old color or strobe
alpha folds into brightness or rate (`brightness × alpha`, as one math node,
or a product of numbers) and alpha is left empty, so saved clips look the
same; a `mirror.offset` other than 0 is refused (none in the 2026-09-30
copy). A line space after a
mirror along the same direction now measures from the plane: the converter
halves its shift and scale (exact when a head sits on the plane, close
otherwise; listed).

---

## 1. Grammar in 10 lines

1. A clip has a name, a selection, a time range, a blend mode, a seed and one graph.
2. A graph has nodes. Exactly one node is an output: `color`, `aim` or `strobe`.
3. Every node has one output wire. A wire goes into an input of another node.
4. An input holds a value (number, vector, color, points, gradient), a wire, or nothing.
5. A choice (`kind`, `wrap`, `base`, `by`) is a setting on a node. It is never wired.
6. Coordinate nodes give a raw 0–1 coordinate: `clock`+`time`, `space`, `noise`, `audio`.
7. `curve` is the only node that turns a coordinate into a value: number, vector or color.
8. Shapers change the head set: `mirror`, `shuffle`, `group`, `split`. They stack.
9. An empty input is the only default: no clock = once over the clip; no heads = all heads; no direction = best fit; no size = one fixture.
10. Overlap is automatic. Per head, the event with the biggest effect shows; ties go to the newest.

Promotion means: an input changes from a value to a wire. Every input is a
tensor over (heads, time, channels). A value has no heads or time axis and is
broadcast. A wire adds time, heads, or both.

Clips follow shaders: each light's value is `f(its field, time)`. A `space`
gives each light its field. There are two ways to move, the two a shader
has, and no other:

1. Shift time per light, `f(t − delay(light))`: every light has its own
   clock. `time.phase` shifts and wraps (loops: wave, spin); `time.delay`
   shifts and does not wrap (one-shots: wipe, cut, bloom, dissolve);
   `time.length` lets each light's clock run for its own length.
2. Shift position over time, `f(p − shift(t))`: `space.shift` slides the
   field (a UV offset) and `space.length` stretches it. A curve over time on
   `shift` moves a shape along the heads with its own ease (chase, bounce,
   sweep); audio on `shift` makes a meter. It is not a band: no width, no
   inside or outside; a pill is a curve over the shifted field.

Curves then play on those clocks and fields. An input with range 0–1 takes a list, and
the list's items multiply (`brightness=[cut, bloom, fade]`).

---

## 2. Node reference

### 2.1 Types and units

Values:

| Type | JSON | Notes |
|---|---|---|
| number | `0.5` | Unit comes from the input. |
| vector | `[u, v, z]` | Stage frame: U right, V downstage, Z up. Direction or metres. |
| color | `[r, g, b]` | Linear Rec. 2020, each 0–1. |
| points | `{"points": [[x, v, ease], ...]}` | The one curve format. `x` 0→1 in order, `v` 0–1, 2–256 points, last point has no ease. Two points may share an `x`: a jump, where `x` itself reads the larger of the two values (ties at an edge count as lit). Three may not. Outside 0–1 the end values hold; a jump at x 0 or x 1 sets that outside value, so `[[0,0],[0,1],[1,1],[1,0]]` is 1 on [0, 1] closed and `[[0,1],[0,0],[1,0]]` is 1 up to 0 closed. |
| list | `[{"node": "curve2"}, 0.5]` | Numbers and number curves, multiplied. Only on a number input with range 0–1 (`"list": true` in its definition). Two or more items, all of one clock. |
| gradient | `{"stops": [{"t": 0, "color": [r,g,b]}, ...]}` | Blends in OKLab. |
| choice | `"line"` | Only in `settings`. |

Units: `share` (0–1), `beats`, `degrees`, `metres`, `hz`, `turns` (0–1),
`uvz` (a vector), `rgb`.

Wire types (one per node kind):

| Wire | From | Into |
|---|---|---|
| clock | `clock` | `time.clock`, `shuffle.clock` |
| heads | `mirror`, `shuffle`, `group`, `split` | any `heads` input |
| coordinate | `time`, `space`, `noise`, `audio` | `curve.x` |
| number | `curve` (kind number) | any number input |
| vector | `curve` (kind vector) | any vector input |
| color | `curve` (kind color) | `color.color` |

A number, vector or color input accepts a value or a wire of its own type.
Nothing else. `brightness=time()` is an error; `brightness=curve(time(), "Ramp up")` is right.

Axes. A wire carries a set of axes: `T` (time), `H` (heads), `E(k)` (the
events of clock `k`). A value carries none. Some inputs refuse an axis; see
each node. `E` is internal: users never see it, the output node resolves it
(section 5.3).

### 2.2 Output nodes

Exactly one per graph. The clip's blend mode stays on the clip row.

**color**

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| color | color | rgb | 0–1 | yes | white `[1,1,1]` |
| brightness | number or list | share | 0–1 | yes | 1 |
| alpha | number or list | share | 0–1 | yes | 1 |

Light per head = color × brightness × alpha; a list is the product of its
items. Blend modes: the light set
(`replace`, `add`, `multiply`, `screen`, `max`, `min`, `lighten`, `value`, `subtract`).

**aim**

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| heads | heads | | | wire only | all clip heads, unmirrored |
| direction | vector | uvz | not zero | yes | `[0, 0.766, -0.643]` |
| point | vector | metres | any | yes | `[0, 0, 0]` |
| yaw | number | degrees | -180–180 | yes | 0 |
| pitch | number | degrees | -180–180 | yes | 0 |
| alpha | number | share | 0–1 | yes | 1 |

Setting `base`: `direction` \| `point` \| `away`. `direction` aims along the
vector. `point` aims each head at the point. `away` aims each head from the
point through the head (a diverging fan; Bloom animates the point's height).
The UI shows only the vector the base uses. Then yaw turns right, pitch turns
up, in the aim's own frame (`aim::offset`). A head that a `mirror` folded
takes the mirror image of yaw and pitch (`aim::mirrored_offset`, one
reflection per stacked mirror, applied in reverse order). Alpha is the weight.
Blend modes: `replace` (`blend_aim`) and `offset` (`offset_aim`), unchanged.
There is no lean.

**strobe**

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| rate | number | share | 0–1 | yes | 0.5 |
| alpha | number | share | 0–1 | yes | 1 |

Shutter = rate × alpha. Blend modes: the light set.

### 2.3 Coordinate nodes

**clock** → clock wire

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| every | number | beats | > 0 | yes (T, H) | error: `clock1.every needs beats, such as every=1` |
| duration | number | beats | > 0 | yes (T, H) | same as every |

Events start at the clip start. Event `k` is born at the beat where the
integrated `every` reaches `k` (the existing clock table handles a varying
`every`). An event lives `duration` beats. Duration may exceed every: events
then overlap on purpose (tails, dense twinkle, per-pill color, turning line).
An event is cut at the clip end.

**time** → coordinate

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| clock | clock | | | wire only | once over the clip |
| delay | number | turns | any | yes | 0 |
| length | number | turns | ≥ 0 | yes | 1 |
| phase | number | turns | any | yes | 0 |

With no clock, `p = (beat − start) / clip duration`, clamped 0–1. With a
clock, `p` is the event's age over its duration, 0–1, one value per live
event (axis `E`). Each head's clock:

```
τ = (p − delay) / length          (length 0: a jump at the delay)
τ = (τ + phase) mod 1              (only when phase ≠ 0)
```

Without phase, τ is below 0 before the head's clock starts and above 1
after it ends; a curve holds its first and last point there, so a shape
that starts with a jump (`"Step up"`, `[[0, 0], [0, 1], [1, 1]]`) is dark
until the head starts. This is the shader form
`smoothstep(delay(p), delay(p) + length(p), t)`. Every head has its own
clock: a wire on phase over heads makes a looping wave or chase; a wire on
delay over heads makes a one-shot (a sweep from a space curve, a dissolve
from a shuffled `order` space); a wire on length over heads makes each
head's fade as long as its field says (a bloom that fades in slower far
from its line).

**space** → coordinate

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| heads | heads | | | wire only | all clip heads |
| direction | vector | uvz | not zero | yes (T) | best fit |
| shift | number | share | any | yes | 0 |
| length | number | share | ≥ 0 | yes | 1 |

Settings: `kind` = `line` \| `order` \| `radial` \| `angle`; `wrap` = yes \| no.
Default `wrap` is no, except `angle`, where it is yes. The UI hides
`direction` for `order`.

The raw axis coordinate `a` of a head, within its span:

- `line`: projection of the head position on `direction`, scaled so the
  lowest head is 0 and the highest 1. Empty direction = the principal axis of
  the span's positions, signed so its largest component is positive (U before
  V before Z on ties). One head, or all at one point: 0.5. With `wrap` yes
  the axis is a ring of the span's `n` units: `a' = (a·(n − 1) + 0.5) / n`,
  so the ends sit one mean spacing apart and never on one place (for evenly
  spaced heads this is the `order` cell).
- `order`: the head's rank in the span's order, cell-centred:
  `(rank + 0.5) / n`. After `shuffle` the rank is the shuffled one.
- `radial`: distance from the span's centroid in the plane, over the largest
  distance; with `wrap` yes, a ring as for `line`. `direction` is the plane normal; empty = the direction of least
  spread (`AxisPlane::Auto` today, with its sign snap).
- `angle`: turns 0–1 around the centroid in that plane (`Mapping::circle` today).

The output is each head's field, the shader's `(p − offset) / scale`:

```
x = (a − shift) / length          (wrap yes: (a − shift) mod 1, then / length)
```

With no shift and no length, `x = a`, 0–1. A space has no band: `x` runs
past 0 and 1 and a curve holds its end values there, or the value a jump at
x 0 or x 1 sets (`[[0, 0], [0, 1], [1, 1], [1, 0]]` is a pill on [0, 1]
closed, dark outside). A region ("the left third") is a curve with jumps
(`[[0, 1], [0.333, 1], [0.333, 0], [1, 0]]`). Motion is a curve over time or
audio on `shift` (the shape moves, form 2), or a curve over the space on a
`time` node's delay, length or phase (each head's clock moves, form 1).
`length` divides rather than multiplies so that it can take a curve (a pill
that grows: `length=curve(time(), "Ramp up")`); `1/length` is not a curve.
A length of 0 is a jump at the shift, as for `time.length`.

**noise** → coordinate

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| heads | heads | | | wire only | all clip heads |
| speed | number | beats | > 0 | yes | 4 |
| scale | number | share | > 0 | yes | uniform (one value for all heads) |
| contrast | number | share | 0–1 | yes | 0 |

One mode. Output 0–1 = coherent value noise sampled at
`(u/scale, v/scale, z/scale, beats/speed)`, where `u, v, z` are the head's
unit position scaled so the span's largest extent is 1. Contrast:
`((raw − 0.5) × (1 + 3·contrast) + 0.5)` clamped. Each noise node has its own
stream: salt = clip seed ⊕ FNV(node id). A tiny scale (0.02) gives each head
its own wander; that replaces `independent`.

**audio** → coordinate

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| low_hz | number | hz | 20–20000 | yes (T) | 40 |
| high_hz | number | hz | 20–20000, > low_hz | yes (T) | 100 |

Output 0–1: the band's energy from the full mix (`band_energy`), scaled over
the clip by its min and max (`clip_range`). Threshold, floor and gain live in
the curve's shape and low/high.

### 2.4 curve → number, vector or color

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| x | coordinate | | | wire only | error: `curve1.x needs a coordinate wire, such as x=time()` |
| shape | points | | v 0–1 | value only | Ramp up `[[0,0],[1,1]]` |
| low | number or vector | the destination's unit | destination range | yes | 0 |
| high | number or vector | the destination's unit | destination range | yes | 1 |
| gradient | gradient | rgb | | value only | required when kind is color |

Setting `kind` = `number` \| `vector` \| `color`. `number` and `vector` take
low/high and refuse gradient. `color` takes gradient and refuses low/high.

Output: `v = shape(x)`, then `low + v × (high − low)` (number and vector, per
component) or `gradient(v)` (color). Below x 0 and above x 1 the shape holds
its end values. Low and high take wires, so curves add: `curve(s, "Ramp up",
low=A, high=A + 1)` is `A + s`. A list multiplies.

The shape is a value: a preset name in Python, points in JSON. The curve
editor and the gradient editor open inside this node. A vector curve that
feeds `direction` or `space.direction` must not pass through zero: low and
high may not be opposite.

### 2.5 Shapers → heads

The heads wire carries, per unit: the member heads, a position (folded by
mirrors), a span id, an order rank (per event after `shuffle`), and the list
of mirror normals that folded it.

**mirror**

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| heads | heads | | | wire only | all clip heads |
| normal | vector | uvz | not zero | yes (T) | best fit: the span's principal axis |
| offset | number | metres | any | yes (T) | 0 |

The plane goes through the middle of the span's extent along the normal, moved
by `offset`. Heads on the low side take their reflected position. Order is
not changed. Aim yaw and pitch are mirrored for those heads. Mirrors stack:
two mirrors give four-fold symmetry (Kaleidoscope).

**shuffle**

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| heads | heads | | | wire only | all clip heads |
| clock | clock | | | wire only | one order for the clip |

A random order of the units within each span, from the clip seed and the
event index (`epoch_seed(seed, index)` ranking, as `SourceOp::Random` today).
With a clock, a new order per event; the output carries `E(k)`. Positions are
not changed. `space(kind=order)` reads the shuffled rank.

**group**

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| heads | heads | | | wire only | all clip heads |
| size | number | heads | ≥ 1 | yes (T, rounded) | one fixture |

Merges heads into units of `size` consecutive heads within a fixture (head
number order). Empty: all heads of a fixture form one unit. A unit acts as
one head: one position (centroid), one rank, one random draw.

**split**

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| heads | heads | | | wire only | all clip heads |

Setting `by` = `fixture` \| `group` (default `fixture`). Each fixture (or each
venue group of the selection) becomes its own span. `space` measures its axis
inside each span; `shuffle` shuffles inside each span; `mirror` folds inside
each span.

### 2.6 Definition JSON

`definitions()` gives one record per kind, used by the UI, Python and the
checker. Shape:

```json
{"kind": "space", "output": "coordinate",
 "inputs": {
   "heads":     {"type": "heads",  "default": null},
   "direction": {"type": "vector", "unit": "uvz",   "default": null, "axes": ["T"]},
   "shift":     {"type": "number", "unit": "share", "default": null, "range": null},
   "length":    {"type": "number", "unit": "share", "default": null, "range": [0, null]}},
 "settings": {
   "kind": {"options": ["line", "order", "radial", "angle"], "default": "line"},
   "wrap": {"options": ["no", "yes"], "default": "no"}}}
```

`default: null` means empty. A number input with range 0–1 also carries
`"list": true`. `Python: space(**{k: v["default"] for k, v in
definition("space")["inputs"].items()})` must pass the checker for every kind.

---

## 3. Type checker

One checker, in Rust (`clip_graph::check`). Score writes, the UI and Python
all run it. Python does not check on its own.

### 3.1 Rules, in order

1. Graph: `version` is 2; 1–64 nodes; each id is an ASCII Python name of at
   most 32 characters (`cut`, `bloom_far`, `curve2`) and not a builder name
   (`clock` … `preset`) or a Python keyword; exactly one output node; every
   other node reaches the output through wires; no cycle.
2. Node: known kind; only its inputs and settings; each setting is one of its
   options.
3. Value: right type for the input (number, vector, color, points, gradient,
   list); finite; in range; points valid (curve rules); gradient valid; a
   direction not zero; a list only on a 0–1 number input, with two or more
   items, each a number in range or a number curve.
4. Wire: right wire type; a curve's kind fits its destination; a curve that
   feeds two inputs has one unit; a curve's low/high in the destination's range
   (interval: `[min(low, high), max(low, high)]`, with wire bounds from their
   own curves).
5. Axes: an input that refuses an axis gets none of it (`group.size`,
   `mirror.offset`, `audio.*_hz`, `space.direction`, `mirror.normal` refuse
   `H`). The inputs of one node together carry at most one clock `E(k)`,
   except the output node, where each input may carry its own clock. The
   items of one list carry at most one clock, also on the output node.
6. Clip: name ≤ 64 chars; blend mode fits the output kind; finite start,
   positive duration.

The heads pipeline needs no venue to check. Geometry errors (an empty
selection) are runtime warnings, not checker errors.

### 3.2 Error format

```
<node id>.<input>: expected <shape and unit>; got <what was given>. Example: <one Python call>
```

Node id is the stored id, so `curve2` in Python is `curve2` in the UI (shown
as "Curve 2"). Examples, verbatim:

- `color1.brightness: expected a number 0–1 (share) or a number curve; got a coordinate wire from time1. Example: brightness=curve(time1, "Ramp up")`
- `curve2.low: expected degrees between -180 and 180 for aim1.yaw; got 400. Example: low=-30`
- `curve3: expected one unit; it feeds aim1.yaw (degrees) and time2.delay (turns). Example: make two curves`
- `curve1.gradient: expected a gradient because kind is color; got nothing. Example: gradient="Rainbow"`
- `aim1.yaw: expected a number -180–180 (degrees) or a number curve; got a list of 2 items. Example: yaw=0. A list multiplies only on an input with range 0–1, such as brightness`
- `time1.phase: expected wires of one clock; got clock2 through curve4 while x follows clock1. Example: use the same clock for both`
- `clock1.every: expected beats above 0; got 0. Example: every=1, or leave the clock out for once over the clip`
- `curve5.low: expected a vector for aim1.direction; got 0. Example: low=(0, 1, -0.5), high=(0, 0.5, -1)`
- `curve5: expected low and high not opposite for space1.direction; got (1,0,0) and (-1,0,0). Example: rotate with kind="angle" instead`
- `graph: expected one output node; got color1 and strobe1. Example: one clip per output`
- `clip: expected a name; got none. Example: name="Kick chase"`
- `noise2.heads: expected a heads wire; got a coordinate wire from space1. Example: heads=group()`

The Rust type is `Error(String)`; the score layer prefixes `clip <name> (<id>): `.

### 3.3 Broadcast at runtime

Tensors are `(heads, time, events)` per channel, as today (`Signal`, events on
the channel axis). Axes broadcast when equal or 1. The checker's axis rules
guarantee that a kernel never sees two different `E`. The 16,777,216 element
cap stays.

---

## 4. Storage

### 4.1 Clip JSON in a score document

```json
{"clips": {"3f24…": {
  "name": "Chase",
  "start": 32.0, "duration": 8.0, "seed": 6348896133488684926,
  "selection": {"expression": "led_bars_vertical"},
  "z_index": 0, "blend_mode": "replace",
  "graph": {
    "version": 2,
    "nodes": {
      "k":      {"kind": "clock", "inputs": {"every": 2}},
      "t":      {"kind": "time",  "inputs": {"clock": {"node": "k"}}},
      "move":   {"kind": "curve", "settings": {"kind": "number"},
                 "inputs": {"x": {"node": "t"}, "shape": {"points": [[0, 0], [1, 1]]}, "low": -0.2, "high": 1}},
      "place":  {"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                 "inputs": {"shift": {"node": "move"}, "length": 0.2}},
      "pill":   {"kind": "curve", "settings": {"kind": "number"},
                 "inputs": {"x": {"node": "place"}, "shape": {"points": [[0, 0], [0, 1], [1, 1], [1, 0]]}}},
      "color1": {"kind": "color", "inputs": {"color": [1, 1, 1], "brightness": {"node": "pill"}}}}}}}}
```

Rules: an empty input is absent. A wire is `{"node": id}`. A list is a JSON
array of numbers and wires; a bare 3-array reads as a vector, a color or a
list of three numbers, by the input's definition. Settings at their
default are still written. `selection_seed` stays as it is. The Rust type:

```rust
pub struct Clip { pub name: String, pub start: f64, pub duration: f64, pub seed: u64,
    pub selection_seed: Option<u64>, pub selection: Selection, pub z_index: i64,
    pub blend_mode: BlendMode, pub graph: ClipGraph }
pub struct ClipGraph { pub version: u32, pub nodes: BTreeMap<String, Node> }
pub struct Node { pub kind: Kind, pub settings: BTreeMap<String, String>, pub inputs: BTreeMap<String, Input> }
pub enum Input { Number(f64), Vector([f64; 3]), Color([f64; 3]), Points(Curve<f64>), Gradient(Gradient), Wire(String), List(Vec<Input>) }
```

`Input` serializes untagged; the definition decides how a bare 3-array reads
(vector or color). `Score { clips }` stays. Both are `deny_unknown_fields`.

### 4.2 Rows

`clips` keeps the row model: one row per clip, per-column last-write-wins,
no revision blobs. Two new columns, in both databases and in
`backend/src/sync/schema.rs`:

```sql
-- backend/migrations/20260929000000_clip_graphs.sql
ALTER TABLE clips ADD COLUMN name TEXT NOT NULL DEFAULT '';
ALTER TABLE clips ADD COLUMN graph_json TEXT NOT NULL DEFAULT '{}';
-- supabase/migrations/20260929000000_clip_graphs.sql
alter table public.clips add column name text not null default '',
                         add column graph_json text not null default '{}';
create schema if not exists backup;
create table backup.clips_pre_clip_graphs_20260929 as select * from public.clips;
create table backup.drafts_pre_clip_graphs_20260929 as select * from public.drafts;
```

`graph` and `inputs_json` stay in place tonight and are not read by new code.
A follow-up migration pair (`20260930000000_drop_form_columns.sql`) drops them
after the upload is verified (section 8.6). Reason: sqlx runs every pending
migration at launch, before the converter could run.

`rows.rs` maps `name` ↔ `clips.name` and `graph` ↔ `clips.graph_json`.
`drafts.base_json` / `state_json` hold whole `Score` documents and convert
with the same converter. The history trigger already covers `clips`; RLS,
the `powersync` publication and the `owned` stream (`SELECT * FROM clips`)
need no change. Redeploy the sync rules after the Postgres migration so
PowerSync picks up the new columns.

Version: `graph.version` in the JSON. A future shape bumps it and converts
once, offline, like this migration. No load-time compatibility layer.

---

## 5. Runtime

### 5.1 Lowering

A clip graph lowers onto the existing substrate: `ClipGraph` → a `Graph` of
kernel nodes in a `Library` → `PreparedGraph` (flatten, fold constants, bake
fixed tables, batch evaluate). `forms::lower` is the template; the new
`clip_graph::lower` replaces it.

Prepare time (cells known):

1. Resolve every heads node to `Units` (members, position, span, order,
   mirror normals) in pipeline order. Constant geometry becomes constant
   `(H,1,1)` fields. A wired `direction`, `normal` or `offset` (time only)
   emits an `Axis` or `Fold` kernel instead of a constant field.
2. Each clock: `ClockTable(every)`, `ClockTable(duration)`, `Events`
   (`once`/`hold` flags removed; a clock is always finite). Outputs
   `progress, present, index, spacing` as today.
3. `time`: `ClipProgress`, or the clock's `progress`, then
   `Shift(delay, length, phase)`.
4. `space`: constant `a` field (or `Axis`).
5. `noise`: `ClockTable(speed)` → `Clock` turns → `Noise4(u,v,z, turns, scale, contrast, salt)`.
6. `audio`: `band_energy` → `clip_range` → `Normalize` → 0–1.
7. `curve`: `Curve(x, low, high; shape, gradient, kind)` → 3 components.
   A list input: its items chained through `Product`.
8. `shuffle` with a clock: `Rank(index, unit keys)` per event → order field with `E`.
9. Output: per clock group, `Pick(present, index, effect factors…)` → winner,
   `Gather(value, winner)` per input; then `ColorOut`, `StrobeOut` or
   `AimOut` (the `SourceOp::Aim` math without lean) → `output` terminal,
   plus the `turn` output for aim.

Kernel list (`clip_graph::kernels`, replacing `forms::source_ops` and
`forms::ops`): `ClockTable`, `Clock`, `Events`, `ClipProgress`, `Shift`,
`Axis`, `Fold`, `Noise4`, `Normalize`, `Curve`, `Rank`, `Pick`,
`Gather`, `Product`, `ColorOut`, `StrobeOut`, `AimOut`. Plus the kept `BandEnergy`,
`ClipRange`, `Output`.

### 5.2 Frame contract

Unchanged: beats in, cells from `Frame.cells`, audio through `FeatureSource`,
12-channel `LightingSignal` out (RGB, dimmer, pan, tilt, strobe, speed, aim
UVZ, weight). `eval/*` and the compositor stay. `compile_clip` and
`prepare_scene_data` call `PreparedGraph::new(library, clip.graph, frame)`
instead of `(library, form id, inputs, frame)`.

### 5.3 Overlap

A wire downstream of clock `k` carries one value per live event of `k`. At
the output node, for each clock `k` and each head and time:

1. Effect per event `e` of `k` = the product of the output's numeric factors
   that carry `E(k)`, at `e`. Factors: color = peak channel of color,
   brightness, alpha; strobe = rate, alpha; aim = alpha, `|yaw| + |pitch|`.
   Factors without `E(k)` do not change the ranking and are skipped.
2. Winner = the present event with the largest effect; ties → the largest
   event index (newest). No present event → all inputs of `k` read 0 (light
   off, alpha 0).
3. Every input that carries `E(k)` is read at the winner. Inputs of another
   clock use their own winner.

`Pick` computes the winner index; `Gather` reads it. This replaces `Latest`
and the winner logic inside `SourceOp::Color`.

### 5.4 Delete and reuse

`backend/crates/patterns/src/`:

| Delete | Reuse (unchanged or trimmed) | New |
|---|---|---|
| `forms/` (all nine files; move `clock_table.rs`, `noise.rs` geometry helpers into `clip_graph/`) | `tensor.rs`, `runtime/mod.rs`, `runtime/lighting.rs`, `prepared.rs`, `prepared/clip_range.rs`, `clip_range.rs` | `clip_graph/mod.rs` (types, JSON) |
| `sources.rs` | `graph.rs` (Graph, Node, Binding, Definition, Library, Primitive; delete `Author`, `Preset`, `Input::author`, `promotable`) | `clip_graph/definitions.rs` (2.6) |
| `recipes.json`, `field_ops.rs`, `signals.rs` (move `coherent_noise` into `clip_graph/noise.rs`), `color.rs::definition`, `envelope.rs` editor-only ops if unused by gpui after phase 4 | `catalog.rs` (registers kernels + `band_energy`, `clip_range`, `output` only) | `clip_graph/check.rs` |
| `runtime/kernels.rs` arms for the generic primitives | `curve.rs`, `color.rs::{Gradient, ColorStop}`, `color_space.rs`, `aim.rs`, `blend.rs`, `clock.rs`, `selection.rs`, `features.rs`, `spatial.rs` (`threshold`, `epoch_seed`), `value_noise.rs`, `inference.rs` | `clip_graph/heads.rs` (Units pipeline; takes `basis`, `least_spread`, `major_axis`, `MirrorPlane::fold`, `unit_direction` from `mapping.rs`) |
| `mapping.rs` types `MappingSpec`, `MappingSource`, `Span`, `AxisPlane`, `Mapping::resolve/leans/mirrored`; `spatial.rs` `Mapping`, `Coordinate` | `value.rs` keeps `Signal, Number, Beats, Proportion, Degrees, Seconds, Color, Vector, Gradient, Lighting`; adds `Points`; deletes the source and mapping variants | `clip_graph/lower.rs`, `clip_graph/kernels.rs` |
| `presets.rs` form presets; `bin/pattern-eval.rs`; `examples/node_reference.rs`; tests `channel_vectors.rs`, `envelopes.rs`, `foundation_compositions.rs`, `signal_output.rs`, `validation.rs` | `score.rs` (`Clip` gains `name`, `graph`; `validate_clip` calls the checker) | `presets.rs` + `presets.json` rewritten (clip presets, curves, gradients, bands) |
| | | `clip_graph/summary.rs` (7.6) |

`backend/src/`: delete `node_graph/lighting.rs` and the `models::node_graph`
arg types the sheet used; keep `node_graph/geometry.rs` (venue binding).
Adapt `services/graph_scores.rs`, `services/composable_patterns.rs::preview`
(previews a preset graph), `eval/lighting.rs::compile_clip`,
`database/local/scores/rows.rs`, `sync/schema.rs`,
`agent_execution/track_host/score.rs` (adds `clip_check`, full 12-channel
render), `agent_execution/bindings/providers/nodes.rs` (13 definitions +
presets binding), `dispatch/handlers/scores.rs` (definitions and presets
queries for the UI).

---

## 6. Python API

Module `luma_exec/clip.py`, exposed as `luma.clip`. The exec namespace binds
these names bare before every cell: `clock, time, space, noise, audio, curve,
mirror, shuffle, group, split, color, aim, strobe, preset`.

### 6.1 Builders

```python
clock(every, duration=None) -> Clock
time(clock=None, delay=0, length=1, phase=0) -> Coordinate
space(heads=None, direction=None, shift=0, length=1, kind="line", wrap=None) -> Coordinate
noise(heads=None, speed=None, scale=None, contrast=None) -> Coordinate
audio(low_hz=None, high_hz=None) -> Coordinate
curve(x, shape=None, low=None, high=None, gradient=None) -> Value
mirror(heads=None, normal=None, offset=None) -> Heads
shuffle(heads=None, clock=None) -> Heads
group(heads=None, size=None) -> Heads
split(heads=None, by="fixture") -> Heads
color(color=None, brightness=None, alpha=None) -> Graph
aim(heads=None, base="direction", direction=None, point=None, yaw=None, pitch=None, alpha=None) -> Graph
strobe(rate=None, alpha=None) -> Graph
preset(name) -> Graph          # a copy of a shipped clip preset, with its name
```

Plain values everywhere: numbers, `(u, v, z)` tuples, `(r, g, b)` triples,
`"#RRGGBB"` (sRGB, converted), a shape as a preset name (`"Comet"`), a list
of points, or `{"points": [...]}`; a gradient as a preset name, a list of
`(t, color)` pairs, or `{"stops": [...]}`. `None` is the empty input.
`curve` sets its kind from its arguments: `gradient` → color; tuple low/high
→ vector; else number. `wrap=None` takes the kind's default. On an input
with range 0–1 a Python list of numbers and curves is a list input: its
items multiply (`color(brightness=[cut, bloom, fade])`).

Variable reuse is linking: the same Python object wired twice is one node.
A node's id is the variable it is assigned to, so code, graph and card say
the same name: `move = curve(t, "Ramp up")` is node `move`, card "move",
and `source()` writes `move = curve(...)` again. When a graph is built,
`color()`, `aim()` and `strobe()` read the caller's variables (innermost
frame first; the first variable bound to a node wins). A node with no
variable, or one whose variable cannot be an id (a builder name, a keyword,
over 32 characters), takes `<kind><n>` in creation order per kind
(`clock1`, `curve3`); the UI shows those as "Clock 1", "Curve 3". A node
loaded from a stored graph keeps its stored id. Renaming is changing the
variable in the code, or renaming the card in the UI; both keep every wire.
Python does not type check; it only refuses unknown keywords.

### 6.2 Clips

```python
edit = luma.track.edit()
clip = edit.add_clip(graph, *, name=None, beats=None, bars=None, seconds=None,
                     selection="all", z=None, blend="replace", seed=None, id=None)
clip = edit.update_clip(clip, *, graph=None, name=None, beats=..., selection=..., z=..., blend=..., seed=...)
edit.remove_clip(clip); edit.check(); edit.diff(); edit.apply(); edit.window(...)
```

`graph` is a `Graph` from `color()/aim()/strobe()`, a `preset("Chase")`, or a
preset name string. `name` is required unless the graph is a preset (then
the preset's name is inherited). `add_clip` and `update_clip` call the host
`track.clip_check` for that one clip at once and raise `ClipError` with the
checker text (3.2), prefixed by the clip name. `check()` and `apply()` check
the whole candidate as today.

Reading back: `Clip` is a frozen dataclass with `id, name, start, duration,
selection, seed, z, blend, graph`. `clip.graph` is a `Graph`:
`graph.nodes` (id → `Node(kind, settings, inputs)`), `graph.output` (the
output node), `graph.json()`, and `graph.source()`, Python text that rebuilds
the same graph (`k = clock(every=2)\nt = time(clock=k)\n...`; the last line
is the output call). `luma.track.definition("space")` returns the definition
record (2.6). `luma.track.nodes()` lists the 13 kinds.

### 6.3 Presets

`luma.presets.clips` (names → `Graph` copies), `luma.presets.curves`
(name → points), `luma.presets.gradients` (name → stops),
`luma.presets.bands` (name → `(low_hz, high_hz)`). A preset name works where
a shape, a gradient or a band is expected: `curve(t, "Comet")`,
`curve(t, "Ramp up", gradient="Fire")`, `audio(*luma.presets.bands["Kick"])`.

### 6.4 window()

`view = edit.window(beats=(32, 40))`. `view.output.values` stays
`[light, time, rgb]`. New: `view.output.aim.values` `[light, time, 3]` unit
vectors in UVZ, `view.output.aim.weight` `[light, time]`,
`view.output.strobe.values` `[light, time]`. The host renders the full
12-channel lighting tensor; `heatmap()` is unchanged.

### 6.5 Before and after

**Chase**

Before:
```python
inputs = defaults("color@1")
inputs["brightness"] = {"type": "space", "value": {
    "axis": {"source": {"kind": "u"}, "per_group": False, "reverse": False},
    "curve": {"points": [[0, 1], [1, 1]]}, "width": {"type": "number", "value": 0.2},
    "width_relative": True, "boundary": "clip",
    "offset": {"type": "time", "value": {"points": [[0, 0], [1, 1]],
        "events": {"every": {"type": "beats", "value": 2}, "life": {"type": "beats", "value": 2}}}}}}
edit.add_clip("color@1", beats=(32, 48), selection="bars", inputs=inputs)
```
After:
```python
k = clock(every=2)
move = curve(time(k), "Ramp up", low=-0.2, high=1)
pill = curve(space(shift=move, length=0.2), [[0, 0], [0, 1], [1, 1], [1, 0]])
edit.add_clip(color(brightness=pill), name="Chase", beats=(32, 48), selection="bars")
```

**Sparkle (Shimmer)**

Before: a `random` brightness with `events {every 0.125, life 0.5}`,
`coverage 0.3`, `level` a Time spike curve.
After:
```python
k = clock(every=0.125, duration=0.5)
lit = curve(space(shuffle(clock=k), kind="order"), [[0, 1], [0.3, 1], [0.3, 0], [1, 0]])
edit.add_clip(color(brightness=[lit, curve(time(k), "Spike")]),
              name="Sparkle", beats=(0, 16), selection="all")
```

**Circle (aim)**

Before: `horizontal` and `vertical` Time sources with sine keyframes, gain 18,
`events {every 4}` and `same_as`.
After:
```python
t = time(clock(every=4))
edit.add_clip(aim(direction=(0, 0.643, -0.766),
                  yaw=curve(t, "Cosine", low=-18, high=18),
                  pitch=curve(t, "Sine", low=-18, high=18)),
              name="Circle", beats=(0, 32), selection="movers", blend="offset")
```

### 6.6 Prompts and skills

`luma.md`, `python-tool.md` and the `node-cards` and `composing-patterns`
skills are rewritten from sections 1, 2, 6 and 9. `node-cards` becomes the
node reference; `composing-patterns` becomes the effect catalog (section 9)
plus the working order. Delete every mention of forms, `inputs=`,
`definition(form)["inputs"]` defaults, sources, `events`, `same_as`, grain,
`width_relative`, lean.

---

## 7. UI

### 7.1 Layout

The inspector (`id("clip-inspector")`, card "Clip graph") is the graph
editor. It is a node-and-wire canvas (decision 28, changed 2026-09-29): one
card per node in columns, sources on the left and the output on the right,
each wire drawn from a node's output port to the input port it feeds. The
layout is automatic and deterministic (`edit::columns`); the stored graph has
no positions. The view pans (drag the ground, or the wheel); "Fit" brings it
back to rest, with the output at the top right. A button widens the
inspector column for the canvas.

Wires: drag from an output port onto an input port to link (a wire the input
cannot take is refused with a message); drag a wire off an input port and
drop it on nothing to unwire (the input goes back to its last value). "Add
node" (button or right-click) places a new node on the canvas; it joins the
graph when its output is wired, with what it needs to check (a coordinate
into a value input comes through a new curve). A card's × deletes the node;
each input it fed goes back to its last value, and a curve whose `x` it was
goes too. A curve card widens its strip.

Top to bottom:

1. Name: `text_input`, full width, agent role `input` "Name". Empty shows the
   summary (7.6) as placeholder.
2. Blend: the existing `blend_select`, filtered by the output kind.
3. The canvas, filling the rest.

A node card: title row with its id: a numbered id shows as the kind in
sentence case plus its number ("Curve 2"), any other id as written ("pill").
Double-click the title, or pick Rename in the card's right-click menu, to
rename the node inline; the checker's id rule applies (and ids are unique),
a refused name shows its error under the title and keeps the old one, and a
rename rewrites every wire to the node as one undo step. Then its settings as `float::segmented()` controls
(`Line | Order | Radial | Angle`, `Wrap` as `float::switch`,
`Direction | Point | Away`, `Fixture | Group`), and a `×` icon button on
non-output cards (delete the node, as above). Then one `sheet_row` per
input, label over control, `ROW_GAP` 14, with the input's port on the card's
left edge. A wired input shows no control; its wire says where its value
comes from.

Controls per input type: number → `ScrubNumber` with the unit (`beats`,
`°`, `m`, `Hz`, `%` for share); vector uvz → three `ScrubNumber` (u, v, z),
and for `direction` the existing `Control::Direction([Turn, Tilt])`; color →
`ColorArgEditor::rgb_only()`; points → `CurveStrip` (`StripValue::Number`)
with `with_presets(curve presets)`, `with_scale([low, high], unit)`,
`over_time(Clock)` when `x` comes from a `time`, `across_space(HeadSource)`
when from a `space`; gradient → `CurveStrip` (`StripValue::Gradient`) with
the gradient presets; a noise node shows `NoisePreview` under its rows.

### 7.2 Add flow and promotion

The timeline insert menu (`picker.rs`) lists the clip presets (searchable,
looping preview) and three blank starts: Color, Aim, Strobe. A new clip is 4
bars at the playhead, as today; duration changes only by resizing on the
timeline.

Each value input row has a source chip in its header (`float::picker_chip`),
reading "Value" when it holds a value. Its menu:

- number, vector, color inputs: `Value`, `Over time`, `Over space`, `Noise`,
  `Audio`, `Link…`. Picking `Over time` inserts `time` (no clock) + `curve`
  wired into the input; `Over space` inserts `space` + `curve`; `Noise`,
  `Audio` likewise. The curve's kind follows the input. `Link…` lists
  compatible nodes already in the graph ("Curve 1 · Ramp up").
- heads inputs: `All`, `Mirror`, `Shuffle`, `Group`, `Split`, `Link…`.
- clock inputs: `Once`, `Clock`, `Link…` (existing clocks).

Defaults on promotion, so the effect is visible at once:

| Destination unit | shape | low | high |
|---|---|---|---|
| share | Ramp up | 0 | the current value, or 1 when it is 0 |
| degrees | Sine | −30 | 30 |
| beats | Ramp down | current ÷ 2 | current |
| metres | Ramp up | current − 1 | current + 1 |
| turns | Ramp up | 0 | 0.5 |
| hz | Ramp up | current ÷ 2 | current × 2 |
| uvz | Ramp up | current | current turned 30° toward Z+ |
| rgb (gradient) | Ramp up | stops: black → current color | |

The new `space` takes an empty direction; the new `clock` takes every 1.
`space.shift` promoted Over time runs from −length to 1 (Ramp up), so the
shape slides fully through.

### 7.3 Edits, undo, tests

Every graph edit goes through one path: `graph_live(clip_id, graph_json)` →
`refresh_clip_preview` → the 250 ms `ARG_FLUSH` debounce → `commit_clips`,
with the burst checkpoint, so one drag is one undo step. Name edits go
through `track_command`. Multi-select: editing is enabled only when every
selected clip has the same graph shape (same kinds, ids, wires); the edit is
applied to all; otherwise the panel reads "N clips" and offers the name field
only.

Agent tree: node cards are `Role::Card` labelled "Curve 2"; input rows are
`Role::Row` labelled by input ("Brightness"); the source chip is `select`
inside the row; segmented options are `button`s. Tests in
`gpui/crates/agent/tests/js/headless/clip_graph.test.js` replace
`clip_forms.test.js`, `aim_sheet.test.js` and the source parts of
`curve_strip.test.js`.

### 7.4 Timeline

`paint_clip` draws the header text as `Name · summary`: the name in the
label weight, the summary in the dimmer detail color (the verb/detail
pattern). Clipped to the box, skipped under 30 px. `document.rs` sets
`label = clip.name`, falling back to the output kind when the name is empty.
Fade handles keep working: they write `alpha` as a value (1) or as
`curve(time(), shape)` with no clock; a wired alpha that is anything else
hides the handles.

### 7.5 Presets browser

`sheet/browser.rs` and `picker.rs` read the clip presets from the new
`presets` query (name, output kind, graph). Tiles preview the graph as
today (`preview_clip` on a stand-in rig).

### 7.6 Summary

`ClipGraph::summary()` → one short string, parts joined by " · ":
for each clock `every {E}` (or `every varies`); for each space its kind word
(`line`, `order`, `radial`, `angle`); `noise` when present; `audio {lo}–{hi} Hz`
when present; `still` when none apply. Example: `line · every 2`.

---

## 8. Migration

One-time, offline, idempotent. Never at load time.

### 8.1 Converter

`backend/scripts/migrate_clip_graphs.py`:

- `--database <copy of luma.db> --out <dir>`: writes `clips.sql`, `drafts.sql`,
  `report.md`, `names.csv`.
- JSON mode: a score document on stdin → converted document on stdout (the
  parity tool uses this).

Every output clip is named. Name = the matching shipped preset when the old
inputs equal a preset's inputs (after conversion, ignoring color values and
constants), else the output kind plus its summary ("Color · line · every 2").

### 8.2 Mapping

Old top level → nodes:

| Old | New |
|---|---|
| `color@1 {color, brightness, fade}` | `color(color, brightness, alpha=fade)` |
| `aim@1 {base, direction, point, lean, horizontal, vertical, axis, fade}` | `aim(base, direction\|point, yaw=horizontal, pitch=vertical, alpha=fade, heads=mirror(axis.mirror) when present)` |
| `strobe.constant@1 {rate, fade}` | `strobe(rate, alpha=fade)` |
| clip `blend_mode`, `seed`, `selection`, `z_index`, `start`, `duration` | unchanged |

Old values → inputs:

| Old value | New |
|---|---|
| `number, proportion, beats, degrees, position, seconds` | number |
| `color`, `vector`, `choice` | color, vector, setting |
| `time {points (numbers), events, phase, gain}` | `clock` (8.3) → `time(clock, phase)` → `curve(kind number, shape = points with v normalized `(v − min)/(max − min)`, low = min × gain, high = max × gain)`. Constant curve (min = max): low = high = v × gain. |
| `time {points (colors), ...}` | color keyframes: the same points with `v = i/(n−1)` and a gradient whose stops are the colors at the points' x, eases kept on the shape. Exact for linear segments; a hold ease stays a hold. |
| `time {gradient, curve, ...}` | `curve(kind color, shape = curve, gradient)` |
| `phase`, `gain` as sources | phase → a wire into `time.phase`; gain → low and high each become a curve with the gain source's coordinate and shape, `low = min × [g_low, g_high]`, `high = max × [g_low, g_high]` |
| `events {same_as: X}` | the same clock node as X (linking) |
| `space {axis, curve\|gradient, offset, width, width_relative, boundary, grain, gain}` | heads = `split(by)` for span fixture/group, then `group(size)` for grain, then `mirror(normal, offset)` for `axis.mirror`; `space(heads, direction, offset', width', kind, wrap = boundary == wrap)`; `curve(space, shape', low = gain × min, high = gain × max)` or a color curve for a gradient |
| `axis.source` | `u`→line (1,0,0); `v`→line (0,1,0); `z`→line (0,0,1); `vector{direction}`→line direction; `major_axis`→line empty direction; `order`→order; `radial`/`angle`→radial/angle with `plane`: auto→empty, up_down→(0,0,1), front_back→(0,−1,0), left_right→(1,0,0), custom→normal; `random`→`shuffle()` + kind order |
| `width`, `width_relative` | `w = width` when absolute. Relative: `gap = min(w × every/duration, 0.8 if overrun else 4)`, `w = gap/(1 − gap)` if overrun else `gap`, with every/duration from the offset's clock at clip start. Overrun = boundary clip and the offset curve has no hold. |
| `offset` (a time source, values `off` 0–1) | start-anchored: `new = off × (1 + w·[overrun]) − w/2·[overrun] − w/2`; applied to the curve's low and high |
| wrapped line stroke crossing once (`boundary wrap`, offset ramp 0 → 1 or 1 → 0, absolute width) | as an overrun stroke: `wrap = no`, offset −w … 1, so the pill enters and leaves instead of starting half on each end (decision 42) |
| shape auto-reverse | when the old offset curve is monotone decreasing, the shape's points are flipped (`x → 1 − x`, eases mirrored); otherwise unchanged and logged as approximate when the shape is asymmetric |
| `random {events, grain, coverage, level}` | `k` (8.3); `h = shuffle(group(size), k)`; `s = space(h, kind order, offset 0, width = coverage)`; `curve(s, "On", low 0, high = level)`; a level source → a wire into `high` |
| `noise {speed, scale, contrast, range, grain, independent, key}` | `n = noise(group(size), speed, scale, contrast)`; `curve(n, "Ramp up", low = range[0], high = range[1])`; `independent` → scale 0.02; node id = old `key` or input path (keeps the salt) |
| `audio {from_hz, to_hz, floor f, threshold th, gain}` | `a = audio(from, to)`; shape `[[0, f], [1, 1]]` when th = 0, else `[[0, 0, "hold"], [th, f + (1 − f)·th], [1, 1]]`; low 0, high = gain |
| aim `lean` constant non-zero | folded into `direction`: `direction' = aim::lean(direction, lean, |lean|)` (exact for a constant direction) |
| aim `lean` = space with curve and gain g (Fan) | `yaw = curve(space(...), shape, low = −g/2, high = g/2)`; approximate (lean toward a stage vector vs yaw in the aim frame) |
| aim `lean` = radial space with growing gain (Bloom) | `base = away`, `point = curve(time(), "Ramp up", low = (c, 0, z + 40), high = (c, 0, z + 6))` with `c, z` the selection centroid; approximate |
| aim `axis.mirror` | `mirror(normal, offset)` into `aim.heads` |
| `fade` (proportion or time over clip) | `alpha` value or `curve(time(), shape)` |

### 8.3 Clocks

| Old `events` | New |
|---|---|
| absent at the root | no clock |
| `every 0`, no life | no clock (once over the clip, hold) |
| `every 0`, life L | `clock(every = clip duration, duration = L)` |
| `every E`, no life | `clock(every = E)` |
| `every E`, life L | `clock(every = E, duration = L)` |
| sources on every/life | wires into `clock.every` / `clock.duration` |
| nested source without events | inherits: the same clock node as its parent |

### 8.4 Unmappable and approximate

Hard (the converter refuses the run and lists the clips): a `space` gain
source on a color gradient; a vector time source with more than two distinct
points whose path is not a straight line; a `time` curve with both numbers
and colors; a clip that fails the new checker after conversion.

Approximate (converted, listed with counts in `report.md`): Fan and Bloom
leans; `independent` noise (a different random stream, same look); spatial
noise (the field is now 4-D; same statistics); relative width with a
varying every or life (taken at clip start); non-monotone offset with an
asymmetric shape (no auto-reverse); overlap winners when color or alpha
varies per event on the brightness clock (the effect now includes them).

### 8.5 Parity

`backend/scripts/check_clip_graphs_migration.py --database <copy> --old <old bin> --new <new bin> --out <dir>`:

1. Old reference: `git archive 6ebfb8f4 | tar -x -C <scratch>/luma-6ebfb8f4`,
   build `patterns/examples/source_parity.rs` there. It dumps frames per clip:
   playback beats at 1/8 beat steps, plus the exclusive end and the
   representable beat before it, on the stand-in rig the old check used.
2. New: `patterns/examples/clip_graph_parity.rs` reads the converted
   document, evaluates the same beats on the same rig, dumps frames.
3. Compare: RGB and strobe within 1e-6, aim direction within 1e-6 rad and
   weight within 1e-6, for every clip not in an approximate class. Approximate
   classes: noise → mean and standard deviation per head within 0.05, and a
   separate list; Fan/Bloom → listed, not compared; audio clips → evaluated
   with the track's decoded mix when the track is present, else schema-checked
   and listed. The 30 shipped presets are compared against their rewritten
   graphs the same way.
4. The report fails on any hard difference. Do not widen the tolerance.

### 8.6 Local and Supabase steps

1. Close `luma-app`. Copy `luma.db`, `luma.db-wal`, `luma.db-shm`, `state.db`
   to `~/.config/com.luma.luma/backups/pre-clip-graphs-20260929/`.
2. Run the converter and the parity check on a copy. Read `report.md`.
3. Apply the Supabase migration (adds columns, takes the backup tables).
   Redeploy `deploy/sync-rules.yaml` (unchanged text) so PowerSync sees the
   new columns.
4. `backend/src/bin/clip_graphs_apply.rs --app-dir ~/.config/com.luma.luma
   --sql <dir>/clips.sql`: opens with `open_app_db_at` so PowerSync queues
   the uploads, runs the SQLite migration, applies the guarded updates
   (`UPDATE clips SET name = ?, graph_json = ? WHERE id = ? AND graph = ? AND
   inputs_json = ?`) in one transaction, and rolls back unless the changed
   row count equals the expected count. Same for `drafts`. Prints
   `ps_crud` and `clips` counts before and after, and runs
   `PRAGMA integrity_check`.
5. Relaunch `luma-app` natively on Sway (never XWayland). Wait until
   `ps_crud` is empty.
6. Verify in Supabase: `select count(*) from clips where graph_json <> '{}'`
   equals `select count(*) from clips` (5,574 on 2026-09-28) and `name <> ''`
   for all; `history` has the update rows; no rejected uploads.
7. Next build: `20260930000000_drop_form_columns.sql` in both databases
   (`ALTER TABLE clips DROP COLUMN graph; ... DROP COLUMN inputs_json;`),
   `schema.rs` drops the two names, redeploy sync rules again.

Ask before step 3 and step 4; Julian approved applying tonight but wants the
report first.


### 8.7 Graph version 2: bands to shifted spaces (2026-09-30)

`backend/scripts/migrate_clip_clocks.py --database <copy> --out <dir>
[--old <v1 clip_graph_parity> --new <v2 clip_graph_parity>]` converts every
version 1 graph in `clips.graph_json` and in `drafts` documents and writes
guarded `UPDATE`s (`clips.sql`, `drafts.sql`, first line `-- expected: N`)
for `clip_graphs_apply`, which queues them for PowerSync, so Supabase
follows the local database. With both binaries it plays every clip before
and after on the stand-in rig (1/8 beat steps) and sorts the clips into
exact, edge-only (a head exactly on a band's end) and approximate.

A version 1 band read `x = (a − offset) / width` (the difference wrapped
first on a ring) and gave the curve's low outside [0, 1]. That is
`space(shift=offset, length=width)` plus a pill edge on the curve. Per space,
with `S` the shape of each curve that reads it:

| v1 band | v2 |
|---|---|
| offset 0, width 1 | drop the inputs |
| offset and width numbers, inside 0..1, not wrapped | `S` placed on [offset, offset + width], 0 outside (jumps); no space inputs |
| any other | `shift` = the offset and `length` = the width, unchanged (numbers or the same wires, eases kept); `S` gains jumps to 0 at x 0 and x 1 where it is not already 0 there |

No node is added, no ease is inverted and ids stay as they are. A color
curve over a band (black outside) is refused (none in the 2026-09-30 copy).
Dry run on a copy, 2026-09-30: 5,609 clips, 0 refused, 0 checker errors;
5,270 play exactly as before; 339 read audio and were checked but not
played (no analyzed track on the stand-in rig); node counts unchanged for
all. Run it on a copy; applying it needs Julian's approval.

---

## 9. Effect catalog

This becomes the `composing-patterns` skill and the shipped clip presets.
Shapes named here are curve presets (`v` 0–1): On `[[0,1],[1,1]]`, Ramp up,
Ramp down, Triangle `[[0,0],[0.5,1],[1,0]]`, Soft (Triangle with Bézier
`[0.4,0,0.6,1]` eases), Comet `[[0,0],[0.95,1],[1,0]]`, Spike
`[[0,0],[0.15,1],[1,0]]`, Drop `[[0,1,"hold"],[0.5,1],[1,0]]`, Swell, Fade in,
Fade out, Square `[[0,1,"hold"],[0.5,0],[1,0]]`, Sine
`[[0,0.5,"sine-out"],[0.25,1,"sine-in"],[0.5,0.5,"sine-out"],[0.75,0,"sine-in"],[1,0.5]]`,
Cosine (Sine shifted a quarter), Double sine (two cycles), Steps 2/3/4/8
(`i/(N−1)` held for `1/N` each). Gradients: Rainbow, Warm, Cool, Fire, Ocean,
Sunset, B/W. Bands: Kick 40–100, Bass 20–250, Mids 250–4000, Highs
4000–16000, Full 20–16000. `D` below is the venue's default aim
`(0, 0.766, -0.643)`.

### Color

| Name | Python | Chain |
|---|---|---|
| Wash | `color(color=(1, 1, 1))` | color |
| Pulse | `beat = clock(every=1); t = time(beat); drop = curve(t, "Drop"); color(brightness=drop)` | clock → time → curve → brightness |
| Breathe | `k = clock(every=4); t = time(k); swell = curve(t, "Swell"); color(brightness=swell)` | clock → time → curve → brightness |
| Fade | `t = time(); fade = curve(t, "Fade in"); color(alpha=fade)` | time → curve → alpha |
| Color fade | `t = time(); hue = curve(t, "Ramp up", gradient=[(0, "#b0400a"), (1, "#2449eb")]); color(color=hue)` | time → curve(color) → color |
| Rainbow | `k = clock(every=4); t = time(k); hue = curve(t, "Ramp up", gradient="Rainbow"); color(color=hue)` | clock → time → curve(color) |
| Gradient | `place = space(); hue = curve(place, "Ramp up", gradient="Sunset"); color(color=hue)` | space → curve(color) |
| Stepped palette | `k = clock(every=4); t = time(k); hue = curve(t, "Steps 4", gradient="Rainbow"); color(color=hue)` | clock → time → curve(color) |
| Two-color swap | `k = clock(every=2); t = time(k); hue = curve(t, "Square", gradient=[(0, "#ff2a00"), (1, "#0040ff")]); color(color=hue)` | clock → time → curve(color) |
| Follows a band | `kick = audio(40, 100); level = curve(kick, "Ramp up"); color(brightness=level)` | audio → curve → brightness |
| VU meter | `bass = audio(20, 250); level = curve(bass, "Ramp up"); bars = split(); height = space(bars, direction=(0, 0, 1), shift=level); meter = curve(height, "Step down"); color(brightness=meter)` | audio → curve → space.shift up each bar; Step down lights the heads below the level |
| Random heads | `k = clock(every=1); order = shuffle(clock=k); rank = space(order, kind="order"); half = curve(rank, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]); color(brightness=half)` | clock → shuffle → space(order) → curve with a jump |
| Random bars | `k = clock(every=1); bars = group(); order = shuffle(bars, clock=k); rank = space(order, kind="order"); half = curve(rank, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]); color(brightness=half)` | clock → shuffle(group) → space → curve |
| Sparkle | `k = clock(every=0.125, duration=0.5); order = shuffle(clock=k); rank = space(order, kind="order"); pick = curve(rank, [[0, 1], [0.3, 1], [0.3, 0], [1, 0]]); t = time(k); spike = curve(t, "Spike"); color(brightness=[pick, spike])` | one clock; a random 30% × a spike over time |
| Build | `order = shuffle(); rank = space(order, kind="order"); start = curve(rank, "Ramp up"); t = time(delay=start); on = curve(t, "Step up"); color(brightness=on)` | shuffle → space(order) → curve → time.delay; time → step |
| Dissolve | `order = shuffle(); rank = space(order, kind="order"); stop = curve(rank, "Ramp down"); t = time(delay=stop); off = curve(t, "Step down"); color(brightness=off)` | as Build, heads go off in a random order |
| Clouds | `cloud = noise(speed=8, scale=0.5); hue = curve(cloud, "Ramp up", gradient="Ocean"); level = curve(cloud, "Ramp up", low=0.2, high=1); color(color=hue, brightness=level)` | one noise → two curves |
| Sparkle rain | `k = clock(every=0.25, duration=1); bars = group(); order = shuffle(bars, clock=k); columns = split(); t = time(k); fall = curve(t, "Ramp up", low=-0.3, high=1); drop = space(columns, direction=(0, 0, -1), shift=fall, length=0.3); streak = curve(drop, "Comet"); rank = space(order, kind="order"); pick = curve(rank, [[0, 0], [0, 1], [0.2, 1], [0.2, 0], [1, 0]]); color(brightness=streak, alpha=pick)` | needs vertical bars; each event's fall is a shift down each bar; the pick is a random 20% per bar |

### Movement

| Name | Python | Chain |
|---|---|---|
| Chase | `k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(shift=move, length=0.2); pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=pill)` | time → curve → space.shift, length 0.2 → a pill that enters and leaves |
| Wave | `k = clock(every=2); place = space(); lag = curve(place, "Ramp down", low=0.5, high=1); t = time(k, phase=lag); swell = curve(t, [[0, 0, [0.4, 0, 0.6, 1]], [0.25, 1, [0.4, 0, 0.6, 1]], [0.5, 0], [1, 0]]); color(brightness=swell)` | Chase with a wide soft pulse |
| Bounce | `k = clock(every=4); t = time(k); move = curve(t, "Triangle", low=0, high=0.8); place = space(shift=move, length=0.2); pill = curve(place, "Soft"); color(brightness=pill)` | a there-and-back time curve on space.shift; a soft pill over the shifted space |
| Comet | `k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(shift=move, length=0.2); tail = curve(place, "Comet"); color(brightness=tail)` | Chase with a Comet shape: a sharp head and a tail |
| Wipe | `k = clock(every=4); place = space(); start = curve(place, "Ramp up"); t = time(k, delay=start); on = curve(t, "Step up"); color(brightness=on)` | space → curve → time.delay; each head turns on in turn and stays on |
| Stepped chase | `k = clock(every=4); t = time(k); move = curve(t, "Steps 4", low=0, high=0.75); place = space(shift=move, length=0.25); block = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=block)` | a Steps 4 curve on space.shift: the block jumps a quarter at a time |
| Many pills | `k = clock(every=0.5, duration=2); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(shift=move, length=0.2); pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=pill)` | Chase with four events alive at once |
| Wrapping chase | `k = clock(every=2); ring = space(wrap=True); lag = curve(ring, "Ramp down"); t = time(k, phase=lag); pill = curve(t, [[0, 0], [0.8, 0], [0.8, 1], [1, 1]]); color(brightness=pill)` | a ring: the pill leaves one end and enters the other |
| Colored pills | `k = clock(every=0.5, duration=2); t = time(k); move = curve(t, "Ramp up", low=-0.25, high=1); place = space(shift=move, length=0.25); age = time(k); hue = curve(age, "Ramp up", gradient="Rainbow"); pill = curve(place, "Soft"); color(color=hue, brightness=pill)` | each event has its own progress, so its own shift and its own color |
| Speed-up chase | `t = time(); every = curve(t, "Ramp down", low=0.25, high=2); life = curve(t, "Ramp down", low=0.5, high=2); k = clock(every=every, duration=life); age = time(k); move = curve(age, "Ramp up", low=-0.4, high=1); size = curve(t, "Ramp down", low=0.1, high=0.4); place = space(shift=move, length=size); tail = curve(place, "Comet"); color(brightness=tail)` | one time() feeds every, duration and the length; the shift runs on each event |
| Alternating sides | `k = clock(every=2); place = space(); lag = curve(place, [[0, 0.5], [0.5, 0.5], [0.5, 0], [1, 0]]); t = time(k, phase=lag); half = curve(t, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]); color(brightness=half)` | the two halves are half a turn apart |
| Diagonal slash | `k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.15, high=1); place = space(direction=(1, 0, 1), shift=move, length=0.15); line = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=line)` | Chase along a diagonal direction |
| Slash | `k=clock(every=2); cut=curve(time(k, delay=curve(space(direction=(-0.82,0,0.57)), "Ramp up", low=0, high=0.2)), "Step up"); d=space(direction=(0.57,0,0.82)); v=[[0,1],[0.68,0],[1,0.47]]; bloom=curve(time(k, delay=curve(d, v, low=0, high=0.9), length=curve(d, v, low=0.05, high=0.4)), "Ramp up"); fade=curve(time(k), [[0,1,"hold"],[0.2,1,"sine-out"],[1,0]]); color(brightness=[cut, bloom, fade])` | not a shipped preset. cut × bloom × fade: a fast cut across one axis, a bloom out from a line with a fade-in that grows with distance, then a fade for all |
| Ripple | `k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.4, high=1); radius = space(shift=move, length=0.4, kind="radial"); ring = curve(radius, "Soft"); color(brightness=ring)` | Chase over radius: rings go out from the centre |
| Wrapping ripple | `k = clock(every=2); radius = space(kind="radial", wrap=True); lag = curve(radius, "Ramp down", low=-0.4, high=0.6); t = time(k, length=0.7143, phase=lag); ring = curve(t, [[0, 0], [0.6, 0, [0.4, 0, 0.6, 1]], [0.8, 1, [0.4, 0, 0.6, 1]], [1, 0]]); color(brightness=ring)` | rings come in again at the centre |
| Spin | `k = clock(every=2); turn = space(kind="angle"); lag = curve(turn, "Ramp down"); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.75, 0], [0.7625, 1], [1, 0]]); color(brightness=arm)` | angle wraps by default |
| Grow | `radius = space(kind="radial"); start = curve(radius, "Ramp up"); t = time(delay=start); on = curve(t, "Step up"); color(brightness=on)` | Wipe over radius: heads turn on from the centre out |
| Turning line | `k = clock(every=2, duration=4); turn = space(kind="angle"); lag = curve(turn, "Ramp down"); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.9, 0], [0.9, 1], [1, 1]]); color(brightness=arm)` | duration = 2 × every: two opposite arms alive |
| Spiral | `k = clock(every=4); radius = space(kind="radial"); inner = curve(radius, "Ramp up"); outer = curve(radius, "Ramp up", low=1, high=2); turn = space(kind="angle"); lag = curve(turn, "Ramp down", low=inner, high=outer); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.7, 0, [0.4, 0, 0.6, 1]], [0.85, 1, [0.4, 0, 0.6, 1]], [1, 0]]); color(brightness=arm)` | phase by angle, moved by radius, bends the arm |
| Mirror | `k = clock(every=2); halves = mirror(); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(halves, shift=move, length=0.2); pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=pill)` | Chase over mirrored heads: pills from both ends meet |
| Kaleidoscope | `k = clock(every=4); sides = mirror(normal=(1, 0, 0)); quarters = mirror(sides, normal=(0, 0, 1)); turn = space(quarters, kind="angle"); lag = curve(turn, "Ramp down"); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.85, 0], [0.8575, 1], [1, 0]]); color(brightness=arm)` | two mirrors, four-fold |

### Aim

| Name | Python | Chain |
|---|---|---|
| Position | `aim(direction=D)` | aim |
| Fan | `place = space(); spread = curve(place, "Ramp up", low=-25, high=25); aim(direction=D, yaw=spread)` | space → curve → yaw |
| Converge | `aim(point=(0, 3, 0), base="point")` | point base |
| Follow | `t = time(); target = curve(t, "Ramp up", low=(-3, 3, 0), high=(3, 3, 0)); aim(point=target, base="point")` | time → curve(vector) → point |
| Bloom | `t = time(); source = curve(t, "Ramp up", low=(0, 0, 40), high=(0, 0, 7)); aim(point=source, base="away")` | away base; the point comes down toward the rig |
| Tunnel | `aim(point=(0, 25, 1.5), base="point")` | a far point downstage |
| Sweep | `k = clock(every=8); t = time(k); swing = curve(t, "Sine", low=-45, high=45); aim(direction=D, yaw=swing)` | clock → time → curve → yaw |
| Nod wave | `k = clock(every=4); place = space(); lag = curve(place, "Ramp up", high=0.6); t = time(k, phase=lag); nod = curve(t, "Sine", low=-25, high=25); aim(direction=D, pitch=nod)` | space → curve → time.phase |
| Circle | `k = clock(every=4); t = time(k); across = curve(t, "Cosine", low=-18, high=18); up = curve(t, "Sine", low=-18, high=18); aim(direction=D, yaw=across, pitch=up)` | one time → two curves |
| Figure-8 | `k = clock(every=4); t = time(k); across = curve(t, "Sine", low=-25, high=25); up = curve(t, "Double sine", low=-12.5, high=12.5); aim(direction=D, yaw=across, pitch=up)` | same, doubled pitch |
| Pinwheel | `k = clock(every=4); turn = space(kind="angle"); lag = curve(turn, "Ramp up"); t = time(k, phase=lag); across = curve(t, "Cosine", low=-20, high=20); up = curve(t, "Sine", low=-20, high=20); aim(direction=D, yaw=across, pitch=up)` | Circle with phase by angle |
| Scissor | `k = clock(every=4); halves = mirror(); t = time(k); swing = curve(t, "Sine", low=-30, high=30); aim(heads=halves, direction=D, yaw=swing)` | mirrored heads yaw the other way |
| Up/down flip | `k = clock(every=2); t = time(k); flip = curve(t, "Square", low=-30, high=30); aim(direction=D, pitch=flip)` | held pitch |
| Ballyhoo | `drift = noise(speed=4, scale=0.02); across = curve(drift, "Ramp up", low=-40, high=40); wander = noise(speed=4, scale=0.02); up = curve(wander, "Ramp up", low=-40, high=40); aim(direction=D, yaw=across, pitch=up)` | two noise nodes, two streams |

Motion presets ship with blend `offset` and a Position underneath is the
usual pairing.

### Strobe

| Name | Python | Chain |
|---|---|---|
| Strobe | `strobe(rate=0.9)` | strobe |
| Ramp | `t = time(); rise = curve(t, "Ramp up"); strobe(rate=rise)` | time → curve → rate |
| Strobe follows a band | `kick = audio(40, 100); rate = curve(kick, "Ramp up", low=0.3, high=1); strobe(rate=rate)` | audio → curve → rate |

### Not one clip

| Name | How |
|---|---|
| Alternating diagonals | Two Diagonal slash clips, directions `(1,0,1)` and `(1,0,-1)`, placed in turn. |
| Fireworks | A Ripple clip and a Sparkle rain clip together. |
| Sky lift | A transition: two Position clips overlap; the second fades in with `alpha=curve(time(), "Fade in")`. |
| Blackout | A Wash with `brightness=0` on `replace`, or nothing placed. |
| Lean over crowd | Position with `direction=(0, 0.94, -0.34)`. |
| Strobe burst | A short Strobe clip. |
| Wobble strobe | Agent's job: measure the wobble rate, place Strobe clips or a Strobe with `clock(every=<rate>)`. |
| Color per pitch | Agent's job: analyse pitch, place Wash clips. |

---

## 10. Build plan

Five agents, one file area each. Nobody edits another agent's files; needs
across areas are met by the contracts in this spec, not by waiting. Format
only touched files. Run only the tests you changed. Commit only your own
files.

| Agent | Owns |
|---|---|
| A patterns | `backend/crates/patterns/**` except `examples/clip_graph_parity.rs` |
| B backend | `backend/src/**` except `agent/prompts/**` and `bin/clip_graphs_apply.rs`; `backend/migrations/*`; `supabase/migrations/*`; `deploy/sync-rules.yaml` |
| C python | `backend/python/luma_exec/**`; `backend/src/agent/prompts/*.md`; `resources/skills/**`; `scripts/headless/mcp_smoke.ts`, `mcp-client.ts` |
| D gpui | `gpui/crates/app/**`; `gpui/crates/ui/**`; `gpui/crates/agent/**` |
| E migration | `backend/scripts/migrate_clip_graphs.py`, `check_clip_graphs_migration.py`; `backend/src/bin/clip_graphs_apply.rs`; `backend/crates/patterns/examples/clip_graph_parity.rs` |

### Phase 1 (A first, 1–2 h): model and checker

A: `clip_graph/{mod,definitions,check,summary}.rs`, JSON round trip,
`Clip { name, graph }`, `presets.json` rewritten from section 9 (clip
presets, curves, gradients, bands), `presets.rs`. Commit as soon as it
compiles so B, C, D read the real types. Tests: JSON round trip for every
preset; one test per checker rule in 3.1 asserting the message of 3.2;
`definitions()` defaults pass the checker for every kind; `summary()` for
three presets.

B, C, D, E start at once from this spec (the JSON in section 4 and the
definitions in 2.6 are the contract).

### Phase 2 (parallel): lowering, backend, python, UI

A: `heads.rs`, `lower.rs`, `kernels.rs`, delete list 5.4, `catalog.rs`
trimmed, `score.rs::validate_clip` → checker + `PreparedGraph::new`. Tests
(behaviour, not constants): every preset lowers and gives non-zero light or
aim on a 12-head stand-in rig; stroke: with offset 0 and width 0.5, exactly
the low half of a line is lit, and with wrap on, a stroke past 1 lights heads
near 0; shuffle: `round(w × n)` units lit, the same set within an event, a
different set in the next; curve outside gives low, color outside gives
black; overlap: two events, the brighter wins, equal brightness → the newer;
two clocks on color and brightness are independent; mirror flips yaw sign
for folded heads and Kaleidoscope's four quadrants match; Turning line lights
two opposite arms; Speed-up chase's event count grows; noise: two nodes with
the same settings differ, one linked node is equal.

B: migrations, `rows.rs`, `schema.rs`, `graph_scores.rs`, `compile_clip`,
`composable_patterns::preview`, dispatch queries `clip_graph_definitions`
and `clip_presets`, `track_host/score.rs` (`clip_check`, 12-channel render
with channel names), `bindings/providers/nodes.rs` (definitions) and a
`presets` binding. Delete `node_graph/lighting.rs`. Tests: row round trip
with name and graph; `save_score` diff writes `graph_json` only when the
graph changed; `clip_check` returns the checker text; `score_render` tensor
has 12 channels.

C: `clip.py`, `score.py`, `track.py` (`output.aim`, `output.strobe`),
`bindings.py` (bare names, `luma.presets`), prompts, skills,
`mcp_smoke.ts` rewritten for the new API. Tests (`luma_exec/tests/`):
builder JSON equals the section 4 example for Chase; linking gives one node;
`graph.source()` round-trips every preset (build → source → exec → equal
JSON); `definition()` defaults build; `add_clip` without a name raises;
`preset("Chase")` inherits the name.

D: `sheet/graph.rs` (new), `sheet.rs`, `browser.rs`, `picker.rs`,
`document.rs`, `track_editor.rs` label, `fades.rs` on alpha; delete
`sheet/form.rs`. Tests (`clip_graph.test.js`): selecting a Chase shows the
name field and the Clock, Time, Curve, Space, Curve and Color cards left to
right with a wire into each input; "Over time" on Brightness of a Wash inserts Time and Curve
cards and the stored graph gains two nodes; switching the space kind segment
stores the setting; dragging a curve point stores new points; a wire dragged
from Clock 1 onto a second time's clock shares one clock; renaming stores
`name` and the timeline header shows it; the fade handle writes an alpha
curve; undo after a drag restores the graph in one step.

E: converter with unit tests per row of 8.2 and 8.3 (fixed JSON in →
expected graph out, then the checker passes), the SQL writer, `clip_graphs_apply`,
`clip_graph_parity.rs`, the old-build export step scripted.

### Phase 3: integrate

1. One agent (B) builds everything: `cargo +1.97.1 check --manifest-path
   gpui/Cargo.toml --workspace --all-targets`, backend crate tests for the
   touched crates, `gpui/test` for `clip_graph.test.js`, the Python tests.
2. E runs the parity check on a copy of the live database and posts
   `report.md`. Zero hard differences is the gate.
3. End-to-end agent run: rebuild `backend/target/debug/luma-mcp`; run
   `scripts/headless/mcp_smoke.ts`, which must: `open` a track, run a cell
   that builds "Kick chase" (`audio` on width, `clock` every 1), `check`,
   read `view.output.aim` and `view.output.values`, `apply`, read the clip
   back and print `clip.graph.source()`, then run a wrong cell
   (`brightness=time()`) and show the 3.2 error text. All steps pass; no
   "skip".
4. Migration steps 8.6.1–8.6.6, with Julian's go at 3 and 4.
5. Relaunch `luma-app` natively on Sway after the final build. Open a score,
   select a migrated Chase, see the graph; scrub playback; resize the clip.
6. Regenerate the www node reference from `definitions()` (follow-up, not
   tonight).

### Acceptance

- Workspace check and all touched tests green.
- Parity report: 0 hard, approximations listed with counts.
- Local: every clip has a name and a `graph_json`; `ps_crud` empty after
  relaunch; `integrity_check` ok.
- Supabase: counts match, history rows present, no rejected uploads.
- MCP end-to-end run passes with the new API and shows one checker error
  with an example.
- `luma-app` running natively, plays the migrated score, inspector shows the
  graph, timeline shows names.

---

## 11. Decision log

One line each; the alternative after "alt:".

1. A stroke is start-anchored: `offset` is where it starts, `x` runs 0→1 to `offset + width`. alt: centre-anchored as today.
2. No overrun heuristic; a pill enters and leaves by setting its offset curve to −width…1. alt: keep the glide-dependent overrun.
3. No auto-reverse of the shape when the offset moves backward; the author flips the shape. alt: keep auto-reverse.
4. `width` is always a share of the axis; `width_relative` is gone. alt: keep relative width.
5. Outside the stroke a number or vector curve gives `low`; a color curve gives black. alt: hold the shape's end value.
6. The order coordinate is cell-centred `(rank + 0.5)/n`, so a shuffled stroke lights `round(w × n)` units exactly as Random did. alt: `rank/(n − 1)`.
7. `curve` has a `kind` setting number|vector|color; gradient only for color, low/high only for number and vector. alt: infer the kind from which inputs are present.
8. `curve` low and high default to 0 and 1 in the destination's unit; vector and color curves must be given low/high or a gradient. alt: no defaults.
9. A curve that feeds inputs of two different units is an error. alt: allow it.
10. Two different clocks may meet only at an output node; anywhere else it is an error. alt: nested event axes.
11. Overlap effect per clock: color = peak(color) × brightness × alpha; strobe = rate × alpha; aim = alpha × (|yaw| + |pitch|); ties newest. alt: brightness only, as today.
12. Aim `base` gains `away` (aim from the point through the head) so Bloom is one animated point. alt: only direction|point and approximate Bloom with pitch over radial.
13. The heads wire carries mirror normals; `aim.heads` uses them to mirror yaw and pitch for folded heads. alt: mirror only folds positions and aim has no heads input.
14. Noise is one 4-D field over normalized (u, v, z) and time; no scale = uniform; salt = clip seed ⊕ FNV(node id). alt: keep the three old modes and `key`.
15. Noise `scale` is a share of the span's largest extent, not metres. alt: metres.
16. `shuffle` permutes order only, within each span, new per event; no clock = once per clip. alt: also shuffle positions.
17. `split` has a setting `by` = fixture|group. alt: fixture only.
18. `group.size`, `mirror.offset`, `mirror.normal`, `space.direction` and `audio.*_hz` are promotable over time only (no heads axis). alt: values only.
19. `clock.every` is required and must be above 0; `every` 0 is an error (use no clock). alt: `every` 0 = once.
20. Empty `space.direction` for line = the span's principal axis, signed so its largest component is positive (U, V, Z on ties); for radial/angle = the least-spread direction with the old sign snap. alt: a `toward` hint input.
21. Storage adds `name` and `graph_json` columns; `graph` and `inputs_json` stay unread until a follow-up migration drops them after the upload is verified. alt: drop them in the same build (sqlx would drop before the converter runs).
22. `graph.version` lives in the JSON; node ids are `<kind><n>`. alt: a version column.
23. The builder names are bound bare in the exec namespace and also under `luma.clip`; bare `time` shadows the stdlib module there. alt: `luma.clip.time` only.
24. `add_clip` and `update_clip` check that one clip at once through `track.clip_check`; `add_clip` without a name and without a preset is an error. alt: check only in `edit.check()`.
25. Python does no type checking; Rust is the one checker. alt: mirror the checks in Python.
26. Shipped clip presets are copied graphs named as the effect; user-saved presets are out of scope tonight. alt: a user preset table now.
27. Preset names are unique across kinds ("Wave" color, "Nod wave" aim, "Strobe follows a band"). alt: names scoped by kind as today.
28. The inspector is a node-and-wire canvas with automatic, deterministic layout and no stored positions (changed 2026-09-29; was a tree of nested cards with link chips). alt: the tree.
29. Promotion inserts nodes with the defaults table in 7.2. alt: empty nodes.
30. The timeline header shows `Name · summary` with the verb/detail colors. alt: name only.
31. Timeline fade handles keep working by writing `alpha` as a value or a once-over-clip curve. alt: remove the handles.
32. Migration approximations (Fan and Bloom leans, independent and spatial noise streams, relative width with varying clocks, asymmetric shapes on non-monotone offsets, overlap winners that now include color and alpha) are accepted and listed, not blockers. alt: block the migration on them.
33. Hard unmappables (gain sources on color gradients, vector paths with more than two distinct points off a line, mixed number/color curves) stop the converter. alt: approximate them too.
34. Old `every 0` with a life L becomes `clock(every = clip duration, duration = L)`. alt: unmappable.
35. The parity reference is built from a `git archive` export of 6ebfb8f4 in the scratch dir, not a worktree. alt: a worktree.
36. `window()` adds `output.aim` and `output.strobe`; `output.values` stays RGB. alt: one 12-channel tensor for callers to slice.
37. A vector curve whose low and high are opposite is rejected statically when it feeds a direction. alt: fall back to best fit at runtime.
38. Multi-select editing works only when the selected clips share one graph shape; otherwise only the name field. alt: per-input intersection like today.
39. Migrated clips are named after the matching shipped preset when their converted graph matches one, else `<Kind> · <summary>`. alt: leave names empty for humans to fill.
40. Sparkle rain ships as a preset marked "needs vertical bars". alt: leave it out of the shipped list.
41. A wrapped `line` or `radial` axis is a ring of the span's n units, `(a·(n − 1) + 0.5)/n`, so its lowest and highest heads never share one place (changed 2026-09-29; before, both read 0 and 1 and lit together as a pill entered). alt: keep 0–1 and light both ends.
42. A wrapped line stroke whose middle crossed the axis once per event migrates unwrapped with offset −w…1; migrated clips are fixed by `backend/scripts/fix_wrapped_strokes.py` (changed 2026-09-29). alt: keep wrap and show half a pill at the far end at every event start.
43. Clips follow shaders: space is a pure field and motion is each head's own clock, `time(delay, length, phase)` (changed 2026-09-30). Decisions 1–4 (strokes, offset, width) are gone with the band. alt: keep bands on space.
44. `time.length` is how long each head's clock runs, in turns of the event (≥ 0, empty 1; 0 is a jump); delay and phase take any number. alt: a rate, or a clamped 0–1 delay.
45. A list on a 0–1 input multiplies its items; it is stored as a list. alt: chain a curve's `high`.
46. Two curve points may share an x (a jump); x itself reads the larger of the two values, so a head exactly on an edge or a moving front is lit (changed 2026-09-30; before, x read the second point and x 0 and x 1 the inner one, so Slash's cut never lit the far corner, which sits exactly where the front stops). A `hold` ease is not a jump: x of the next point reads that point. alt: hold eases and 0.001 offsets; read the side the front comes from.
47. "VU meter" and "Bounce" are presets again (changed 2026-09-30): the VU meter shifts the space by the audio level under a step; Bounce shifts it by a there-and-back time curve. alt: leave them out.
48. `space.shift` and `space.length` give `(a − shift) / length`, the shader's UV offset and scale; motion is either a per-head clock (`time`) or a shifted field (`space`), and nothing else (2026-09-30). Length divides so a moving width stays one curve. alt: a delay per head for every sweep, which needs inverted eases and curve chains.
49. A node's id is its Python variable name (any ASCII Python name up to 32 characters, not a builder name or keyword); unnamed nodes are `<kind><n>`; the card shows the id as-is, or "Curve 2" for a numbered id; the UI renames by editing the card title and rewrites every wire (2026-09-30). alt: numbered ids only.
