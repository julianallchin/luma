---
name: node-cards
description: The clip graph node reference. The grammar in ten lines, every one of the 13 nodes (math included) with its inputs, units, ranges, empty values and settings, the wire types, the curve presets, and the checker's error format. Read this before you build a clip graph.
---

# Clip graph nodes

Every clip has one small graph. You write it as Python with bare builder
functions. The Rust checker reads the same graph that the inspector shows.

## Grammar in 10 lines

1. A clip has a name, a selection, a time range, a blend mode, a seed and one graph.
2. A graph has nodes. Exactly one node is an output: `color`, `aim` or `strobe`.
3. Every node has one output wire. A wire goes into an input of another node.
4. An input holds a value (number, vector, color, points, gradient), a wire, or nothing.
5. A choice (`kind`, `wrap`, `base`, `by`) is a setting on a node. It is never wired.
6. Coordinate nodes give a raw coordinate per head: `time`, `space`, `noise`, `audio`.
7. `curve` is the only node that turns a coordinate into a value: number, vector or color. `math` combines values.
8. Shapers change the head set: `mirror`, `shuffle`, `group`, `split`. They stack.
9. An empty input is the only default: no `every` = once over the clip; no heads = all heads; no direction = best fit; no size = one fixture.
10. Overlap is automatic. The live events of one `time(every=...)` make one layer: color and strobe keep the largest per channel, aim puts the newest on top. Between events the clip is not there (alpha 0).

**Black or transparent.** Brightness 0 is black light: in `replace` it
covers the light below with black. Alpha 0 is no clip: the light below
shows. Gaps between events and outside the clip are alpha 0.

Promotion means: an input changes from a value to a wire. Every number,
vector and color input can take a wire.

A value is a function of the head's place and of time, as in a shader:
`space()` gives the place, `time()` gives the time, and a curve turns them
into a value. There are two ways to move:

- **Shift time per head.** Put a curve over space into `time.delay` or
  `time.phase`: each head runs the same shape on its own clock (a wave, a
  wipe, a spin).
- **Shift the place over time.** Put a curve over time (or audio) into
  `space.shift` or `space.scale`: the shape slides or grows along the heads
  with its own ease (a chase, a bounce, a bloom, a meter).

To make a region, put a curve with a jump over space. To combine two
patterns, multiply them: `brightness=cut * bloom * fade`.

A node's id is the variable you assign it to: `move = curve(t, "Ramp up")`
is node `move`, and its card in the inspector reads "move". A node without
a variable gets `<kind><n>` ("Curve 2"). `graph.source()` writes the ids
back as variables, so a round trip keeps the names. It writes a curve or a
math with a numbered id that one input reads in place:
`space(shift=curve(t, 'Ramp up'))`, `color(brightness=cut * fade)`. An id is
a Python name of at most 32 characters that is not a builder name (`time`,
`space`, `noise`, `audio`, `curve`, `math`, `mirror`, `shuffle`, `group`,
`split`, `color`, `aim`, `strobe`, `preset`, `max`, `min`) or a keyword.

## Values and units

| Type | Python | Notes |
|---|---|---|
| number | `0.5` | The unit comes from the input. |
| vector | `(u, v, z)` | Stage frame: U right, V downstage, Z up. A direction or metres. |
| color | `(r, g, b)` or `"#RRGGBB"` | Linear Rec. 2020, each 0–1. Hex is sRGB and is converted. |
| points | `"Comet"`, `[[x, v], [x, v, ease], ...]` | A curve preset name or points. |
| gradient | `"Fire"`, `[(t, color), ...]` | A gradient preset name or stops. Blends in OKLab. |
| choice | `kind="order"` | A setting. |

Units: share (0–1), beats, degrees, metres, hz, turns, uvz (a vector), rgb.

Points: `x` goes from 0 to 1 and increases. Two points can have the same `x`:
that is a jump. At the jump's `x` the curve reads the value after the jump,
as a shader's `step`: each piece covers [a, b), at 0 and 1 too. So
`[[0, 0], [0, 1], [1, 1], [1, 0]]` is 1 from 0 up to 1 (1 itself is 0),
and `[[0, 1], [0, 0], [1, 0]]` is 1 below 0 only.
`[[0, 1], [0.5, 1], [0.5, 0], [1, 0]]` is on below 0.5 and off from 0.5.
A front lights a head once it has reached it when x = front − place and
the curve steps up at 0 (`"Step up"`).
`v` is 0–1. A curve has 2–256 points. The ease says how the value moves to the next point: `linear`
(no ease), `hold`, `ease-in`, `ease-out`, `ease-in-out`, `sine-in`,
`sine-out`, `sine-in-out`, or a local cubic Bézier `[x1, y1, x2, y2]`
(x1 and x2 in 0–1; y1 and y2 may go outside 0–1 to overshoot, as in CSS).
Between points a value may leave 0–1; the outputs clamp. The last point has
no ease.

A 3-tuple on `color.color` or a vector input is a color or a vector. A list
of curves is an error: to multiply, write `brightness=cut * fade`.

## Wires

| Wire | From | Into |
|---|---|---|
| time | `time` | `shuffle.time` (and `curve.x`) |
| heads | `mirror`, `shuffle`, `group`, `split` | any `heads` input |
| coordinate | `time`, `space`, `noise`, `audio` | `curve.x` |
| number | `curve` (number), `math` | any number input, a curve's `low`/`high` |
| vector | `curve` (vector), `math` | any vector input, a curve's `low`/`high` |
| color | `curve` (color), `math` | `color.color` |

A number, vector or color input takes a value or a wire of its own type. A
coordinate never goes straight into a number input:
`brightness=time()` is an error; `brightness=curve(time(), "Ramp up")` is right.

The same Python variable wired twice is one node (a link). Two calls with
the same arguments are two nodes. Ids are `<kind><n>` in creation order per
kind: `time1`, `curve3`, `math1`. The UI and the errors use the same ids.

## Output nodes

Exactly one per graph. The blend mode is on the clip, not in the graph.

**color** `color(color=None, brightness=None, alpha=None)`

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| color | color | rgb | 0–1 | white `(1, 1, 1)` |
| brightness | number | share | 0–1 | 1 |
| alpha | number | share | 0–1 | 1 |

Light per head = color × brightness. `brightness` is the pattern across the
lights: a chase, a pulse, a cut. `alpha` is the clip's opacity: the clip's
light blends with the light below by the blend mode, then the result mixes
with the light below by alpha. Alpha 0 shows the light below, in every
blend mode. Use alpha for a fade of the whole clip (the timeline fade points
edit it). Blend modes: `replace` (the clip's light, as is), `add`,
`multiply`, `screen`, `max`, `min`, `subtract`.

**aim** `aim(heads=None, base="direction", direction=None, point=None, yaw=None, pitch=None, alpha=None)`

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| heads | heads | | | all clip heads |
| direction | vector | uvz | not zero | `(0, 0.766, -0.643)` |
| point | vector | metres | any | `(0, 0, 0)` |
| yaw | number | degrees | -180–180 | 0 |
| pitch | number | degrees | -180–180 | 0 |
| alpha | number | share | 0–1 | 1 |

Setting `base`: `direction` aims along the vector. `point` aims each head at
the point. `away` aims each head from the point through the head (a fan that
opens). Then yaw turns right and pitch turns up, in the aim's own frame. A
head that a `mirror` folded takes the mirror image of yaw and pitch. Alpha
is the weight. Blend modes: `replace` sets the aim; `offset` adds yaw and
pitch to the aim underneath.

**strobe** `strobe(rate=None, alpha=None)`

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| rate | number | share | 0–1 | 0.5 |
| alpha | number | share | 0–1 | 1 |

Shutter = rate. `alpha` is the clip's opacity over the strobe below, as on
color. Blend modes: the same as color.

## Coordinate nodes

**time** `time(every=None, duration=None, delay=None, phase=None)` → coordinate

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| every | number | beats | above 0 | once over the clip, no events |
| duration | number | beats | above 0 | every (the clip with no every) |
| delay | number | beats | any | 0 |
| phase | number | turns | any | 0 |

With `every`, events start at the clip start, one each `every` beats. Each
event lives `duration` beats. A duration above every makes events overlap on
purpose: tails, many pills, a color per pill, a turning line with two arms.
The clip end cuts an event. With no every and a duration, there is one event
of that length from the clip start. Two time nodes whose every and duration
are equal (the same numbers or the same wires) share one set of events.

`p` is the progress: the age of the event (or of the clip) over its
duration, 0–1. Each head then gets its own clock: τ = p − delay / duration.
If phase is given (0 too, any number), τ = fract(τ + phase).

- **delay** is in beats and does not wrap. Before a head's start τ is below
  0; a curve holds its first value there (a waiting head of a dissolve stays
  on). After the end a curve holds its last value. Use delay for one-shots: a
  wipe, a cut, a dissolve, a build.
- **phase** is in turns and wraps, so the clock loops. Use phase for loops: a
  chase, a wave, a spin.

Put a curve over space into delay or phase to make heads differ.
`time(every=4, delay=curve(space(), "Ramp up", high=2))` starts each head
later along the axis, up to 2 beats. The `low` and `high` of that curve set
the spread: beats for delay, turns for phase.

**space** `space(heads=None, direction=None, at=None, shift=None, scale=None, kind="line", wrap=None)` → coordinate

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| heads | heads | | | all clip heads |
| direction | vector | uvz | not zero | best fit |
| at | vector | share | each 0–1 | the middle `(0.5, 0.5, 0.5)` |
| shift | number | share | any | 0 |
| scale | number | share | 0 or more (0 is a jump) | 1 |

The place of each head `a`, 0–1 within the selection (within each span after
a `split`), then `x = (a − shift) / scale`. With `wrap`, x tiles as a
shader's `fract`: `x = fract((a − shift) / scale)`, so the shape repeats
every `scale` (scale 0.25 = four copies across the heads). `shift` slides the place (the shader's UV offset): a curve over
time on shift moves the shape along the heads, with the ease of that time
curve. `scale` is how much of the axis reads as 0–1; a curve over time on
scale grows the shape (a bloom). There is no band: past 0 and 1 a curve holds its end values,
so give a moving pill jumps at its ends, `[[0, 0], [0, 1], [1, 1], [1, 0]]`.

Setting `kind`:
- `line`: the position along `direction`, lowest head 0, highest 1. Empty
  direction = the stage axis (+U, +V or +Z) the heads spread along most. After a `mirror` whose normal is
  parallel to the direction, 0 is on the mirror's plane and the place grows
  away from it, by distance over the span's full extent (so 0.5 at the edge
  for a plane in the middle).
- `order`: the rank of the head, `(rank + 0.5) / n`. After `shuffle` it is
  the shuffled rank.
- `radial`: distance from the centre over the largest distance: 0 at the
  centre, 1 at the farthest head. The centre is `at`, each of u, v, z 0–1
  within the selection's box; empty = its middle. `direction` is the plane
  normal. A shift moves rings outward from the centre.
- `angle`: turns 0–1 around the centre `at`.

Setting `wrap`: `True` or `False`. Empty = `False`, except `angle` (`True`).

A static region is a curve over space with a jump:
`curve(space(), [[0, 1], [0.5, 1], [0.5, 0], [1, 0]])` lights the first half.
A moving region is a curve over the space whose `shift` is a curve over
time: `move = curve(time(every=2), "Ramp up", low=-0.2, high=1)` then
`curve(space(shift=move, scale=0.2), [[0, 0], [0, 1], [1, 1], [1, 0]])`
enters at one end and leaves at the other. To run the other way, use
"Ramp down" on the move, or the reverse `direction`.

A wrapped axis (`wrap=True`, and `angle`) is a ring: its ends do not meet,
so a phase curve over it loops with no seam. One pill per turn of the ring
is a narrow curve with scale 1 (`[[0, 0], [0, 1], [0.2, 1], [0.2, 0], [1, 0]]`),
not a smaller scale, which tiles. A curve over time on a wrapped scale
zooms: `scale` 0.5 → 0.125 turns 2 copies into 8.

**noise** `noise(heads=None, speed=None, scale=None, contrast=None)` → coordinate

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| heads | heads | | | all clip heads |
| speed | number | beats | above 0 | 4 |
| scale | number | share | above 0 | one value for all heads |
| contrast | number | share | 0–1 | 0 |

Smooth noise 0–1 over head position and time. `scale` is a share of the
rig's largest extent: 0.5 gives slow clouds, 0.02 gives each head its own
wander. Each noise node has its own stream.

**audio** `audio(low_hz=None, high_hz=None)` → coordinate

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| low_hz | number | hz | 20–20000 | 40 |
| high_hz | number | hz | above low_hz | 100 |

The energy of the band in the full mix, scaled 0–1 by its lowest and highest
value over the whole track: every clip reads the same level at the same
moment. `audio("Kick")` takes a band preset. Put a threshold or a
floor in the curve's shape and low/high. It needs track analysis.

## curve

`curve(x, shape=None, low=None, high=None, gradient=None)` → number, vector or color

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| x | coordinate | | | error: give it |
| shape | points | | v 0–1 | Ramp up |
| low | number or vector | the destination's | the destination's | 0 |
| high | number or vector | the destination's | the destination's | 1 |
| gradient | gradient | rgb | | needed for a color curve |

Output: `low + shape(x) × (high − low)`, or `gradient(shape(x))`. The kind
comes from the arguments: a gradient makes a color curve; tuple low/high
make a vector curve; all else is a number curve. A vector curve must get
low and high. A curve that feeds two inputs must feed inputs of one unit. A
vector curve that feeds a direction must not have opposite low and high.
`low` and `high` can be wires from another curve or a math node.

## math

Python operators on values (curve and math results) make math nodes:

| Python | op | Items |
|---|---|---|
| `a * b * c` | `*` | 2 or more |
| `a + b` | `+` | 2 or more |
| `a - b` | `-` | exactly 2 |
| `max(a, b)` | `max` | 2 or more |
| `min(a, b, 0.2)` | `min` | 2 or more |

An item is a number or a value wire: `0.5 * fade`, `1 - fade`. A chain of
one operator is one node: `cut * bloom * fade` is one math node with three
items. A named step is its own node: `glow = cut * fade` is node `glow`.
The output is a value of the widest item kind (color > vector > number; a
number times a color is a color; color with vector is an error). A math
result wires like a curve: into any number, vector or color input, or into a
curve's `low` or `high`. A coordinate is not a value: `time() * 2` is an
error; write `curve(time(), low=0, high=2)`. On plain numbers `max` and `min`
are Python's own.

## Shapers

**mirror** `mirror(heads=None, normal=None, at=None)` → heads

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| heads | heads | | | all clip heads |
| normal | vector | uvz | not zero | best fit |
| at | number | share | 0–1 | 0.5 |

Folds the heads across a plane. `at` places the plane along the normal,
across the positions of the selection before any fold: 0.5 is always the
centre, also for stacked mirrors. Heads on the low side reflect. Order does
not change. Aim yaw and pitch mirror for folded heads. Two mirrors give four-fold
symmetry.

**shuffle** `shuffle(heads=None, time=None)` → heads

A random order of the heads, from the clip seed. `time` is a wire from a
time node: with `every`, a new order per event; without, one order.
Positions do not change. Read it with `space(kind="order")`.
`curve(space(shuffle(time=time(every=1)), kind="order"), [[0, 1], [0.3, 1], [0.3, 0], [1, 0]])`
lights a random 30% of the heads each beat.

**group** `group(heads=None, size=None)` → heads

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| heads | heads | | | all clip heads |
| size | number | heads | 1 or more | one fixture |

Merges heads into units of `size` heads within a fixture. A unit acts as one
head: one position, one rank, one random draw.

**split** `split(heads=None, by="fixture")` → heads

Setting `by`: `"fixture"` or `"group"`. Each fixture, or each venue group of
the selection, becomes its own span. `space`, `shuffle` and `mirror` then
work inside each span: one bar meter per bar, for example.

`group.size`, `mirror.normal`, `mirror.at`, `space.direction` and
`audio.*_hz` can change over time, but not across heads.

## Two clocks

A clock is one set of events: a time node with `every`, or time nodes with
equal every and duration. The inputs of one node carry at most one clock,
except the output node. So `color(color=<clock A>, brightness=<clock B>)` is
right, but a curve whose `x` follows clock A and whose `low` follows clock B
is an error, and so is `a * b` with a and b on two clocks.

## Presets

Curves (`v` 0–1): On, Ramp up, Ramp down, Step up, Step down, Triangle, Soft, Comet, Spike,
Drop, Swell, Fade in, Fade out, Square, Sine, Cosine, Double sine, Steps 2,
Steps 3, Steps 4, Steps 8. "Step up" is `[[0, 0], [0, 1], [1, 1]]`: 0 below
x = 0, then 1. "Step down" is its reverse. Gradients: Rainbow, Warm, Cool, Fire, Ocean,
Sunset, B/W. Bands: Kick 40–100, Bass 20–250, Mids 250–4000, Highs
4000–16000, Full 20–16000. Read the exact values from `luma.presets.curves`,
`luma.presets.gradients` and `luma.presets.bands`. Clip presets are in
`luma.presets.clips`; `preset("Chase")` gives a copy with its name.

`luma.track.definition("space")` gives one node's definition record: inputs
with type, unit, range and default (`None` is empty), and settings with
their options. `luma.track.nodes()` lists the 13 kinds.

## Errors

The checker runs when you add or update a clip. It raises `ClipError`:

```
<node id>.<input>: expected <shape and unit>; got <what was given>. Example: <one Python call>
```

Examples:

- `color1.brightness: expected a number 0–1 (share) or a number curve; got a coordinate wire from time1. Example: brightness=curve(time1, "Ramp up")`
- `curve2.low: expected degrees between -180 and 180 for aim1.yaw; got 400. Example: low=-30`
- `curve3: expected one unit; it feeds aim1.yaw (degrees) and time1.phase (turns). Example: make two curves`
- `math1.values: expected exactly two items for -; got 3. Example: values=[curve1, curve2]`
- `curve1.gradient: expected a gradient because kind is color; got nothing. Example: gradient="Rainbow"`
- `time1.every: expected beats above 0; got 0. Example: every=1, or leave every out for once over the clip`
- `graph: expected one output node; got color1 and strobe1. Example: one clip per output`
- `clip: expected a name; got none. Example: name="Kick chase"`

Do the example, then add the clip again.
