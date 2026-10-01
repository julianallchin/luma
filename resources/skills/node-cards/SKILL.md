---
name: node-cards
description: The clip graph node reference. The grammar in ten lines, every one of the 13 nodes with its inputs, units, ranges, empty values and settings, the wire types, the curve presets, and the checker's error format. Read this before you build a clip graph.
---

# Clip graph nodes

Every clip has one small graph. You write it as Python with bare builder
functions. The Rust checker reads the same graph that the inspector shows.

## Grammar in 10 lines

1. A clip has a name, a selection, a time range, a blend mode, a seed and one graph.
2. A graph has nodes. Exactly one node is an output: `color`, `aim` or `strobe`.
3. Every node has one output wire. A wire goes into an input of another node.
4. An input holds a value (number, vector, color, points, gradient), a wire, a list (on 0–1 inputs), or nothing.
5. A choice (`kind`, `wrap`, `base`, `by`) is a setting on a node. It is never wired.
6. Coordinate nodes give a raw coordinate per head: `clock`+`time`, `space`, `noise`, `audio`.
7. `curve` is the only node that turns a coordinate into a value: number, vector or color.
8. Shapers change the head set: `mirror`, `shuffle`, `group`, `split`. They stack.
9. An empty input is the only default: no clock = once over the clip; no heads = all heads; no direction = best fit; no size = one fixture.
10. Overlap is automatic. Per head, the event with the biggest effect shows; ties go to the newest.

Promotion means: an input changes from a value to a wire. Every number,
vector and color input can take a wire.

A value is a function of the head's place and of time, as in a shader:
`space()` gives the place, `time()` gives the time, and a curve turns them
into a value. There are two ways to move:

- **Shift time per head.** Put a curve over space into `time.delay`,
  `time.length` or `time.phase`: each head runs the same shape on its own
  clock (a wave, a wipe, a spin).
- **Shift the place over time.** Put a curve over time (or audio) into
  `space.shift`: the shape slides along the heads with its own ease (a
  chase, a bounce, a sweep, a meter).

To make a region, put a curve with a jump over space.

A node's id is the variable you assign it to: `move = curve(t, "Ramp up")`
is node `move`, and its card in the inspector reads "move". A node without
a variable gets `<kind><n>` ("Curve 2"). `graph.source()` writes the ids
back as variables, so a round trip keeps the names. An id is a Python name
of at most 32 characters that is not a builder name or a keyword.

## Values and units

| Type | Python | Notes |
|---|---|---|
| number | `0.5` | The unit comes from the input. |
| vector | `(u, v, z)` | Stage frame: U right, V downstage, Z up. A direction or metres. |
| color | `(r, g, b)` or `"#RRGGBB"` | Linear Rec. 2020, each 0–1. Hex is sRGB and is converted. |
| points | `"Comet"`, `[[x, v], [x, v, ease], ...]` | A curve preset name or points. |
| gradient | `"Fire"`, `[(t, color), ...]` | A gradient preset name or stops. Blends in OKLab. |
| list | `[cut, bloom, 0.5]` | Only on a number input with range 0–1. The items multiply. |
| choice | `kind="order"` | A setting. |

Units: share (0–1), beats, degrees, metres, hz, turns, uvz (a vector), rgb.

Points: `x` goes from 0 to 1 and increases. Two points can have the same `x`:
that is a jump. At the jump's `x` the curve reads the second point. A jump
at x 0 or x 1 sets the value outside 0–1; x 0 and x 1 themselves read the
inner point, so `[[0, 0], [0, 1], [1, 1], [1, 0]]` is 1 from 0 to 1, both
ends included, and 0 outside.
`[[0, 1], [0.5, 1], [0.5, 0], [1, 0]]` is on up to 0.5 and off after it.
`v` is 0–1. A curve has 2–256 points. The ease says how the value moves to the next point: `linear`
(no ease), `hold`, `ease-in`, `ease-out`, `ease-in-out`, `sine-in`,
`sine-out`, `sine-in-out`, or a local cubic Bézier `[x1, y1, x2, y2]`. The
last point has no ease.

Lists: on an input with range 0–1 (`color.brightness`, `color.alpha`,
`aim.alpha`, `strobe.rate`, `strobe.alpha`, `noise.contrast`), a list of two
or more numbers and number curves is their product:
`color(brightness=[cut, bloom, fade])`. The items must follow one clock. On
any other input a list is an error. A 3-tuple on `color.color` or a vector
input is a color or a vector, not a list.

## Wires

| Wire | From | Into |
|---|---|---|
| clock | `clock` | `time.clock`, `shuffle.clock` |
| heads | `mirror`, `shuffle`, `group`, `split` | any `heads` input |
| coordinate | `time`, `space`, `noise`, `audio` | `curve.x` |
| number | `curve` (number) | any number input |
| vector | `curve` (vector) | any vector input |
| color | `curve` (color) | `color.color` |

A number, vector or color input takes a value or a wire of its own type. A
coordinate never goes straight into a number input:
`brightness=time()` is an error; `brightness=curve(time(), "Ramp up")` is right.

The same Python variable wired twice is one node (a link). Two calls with
the same arguments are two nodes. Ids are `<kind><n>` in creation order per
kind: `clock1`, `curve3`. The UI and the errors use the same ids.

## Output nodes

Exactly one per graph. The blend mode is on the clip, not in the graph.

**color** `color(color=None, brightness=None, alpha=None)`

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| color | color | rgb | 0–1 | white `(1, 1, 1)` |
| brightness | number or list | share | 0–1 | 1 |
| alpha | number or list | share | 0–1 | 1 |

Light per head = color × brightness × alpha. Blend modes: `replace`, `add`,
`multiply`, `screen`, `max`, `min`, `lighten`, `value`, `subtract`.

**aim** `aim(heads=None, base="direction", direction=None, point=None, yaw=None, pitch=None, alpha=None)`

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| heads | heads | | | all clip heads |
| direction | vector | uvz | not zero | `(0, 0.766, -0.643)` |
| point | vector | metres | any | `(0, 0, 0)` |
| yaw | number | degrees | -180–180 | 0 |
| pitch | number | degrees | -180–180 | 0 |
| alpha | number or list | share | 0–1 | 1 |

Setting `base`: `direction` aims along the vector. `point` aims each head at
the point. `away` aims each head from the point through the head (a fan that
opens). Then yaw turns right and pitch turns up, in the aim's own frame. A
head that a `mirror` folded takes the mirror image of yaw and pitch. Alpha
is the weight. Blend modes: `replace` sets the aim; `offset` adds yaw and
pitch to the aim underneath.

**strobe** `strobe(rate=None, alpha=None)`

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| rate | number or list | share | 0–1 | 0.5 |
| alpha | number or list | share | 0–1 | 1 |

Shutter = rate × alpha. Blend modes: the same as color.

## Coordinate nodes

**clock** `clock(every, duration=None)` → clock wire

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| every | number | beats | above 0 | error: give it |
| duration | number | beats | above 0 | the same as every |

Events start at the clip start, one each `every` beats. Each event lives
`duration` beats. A duration above every makes events overlap on purpose:
tails, many pills, a color per pill, a turning line with two arms. The clip
end cuts an event. For once over the clip, use no clock.

**time** `time(clock=None, delay=0, length=1, phase=0)` → coordinate

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| clock | clock | | | once over the clip |
| delay | number | turns | any | 0 |
| length | number | turns | 0 or more (0 is a jump) | 1 |
| phase | number | turns | any | 0 |

`p` is the progress: with no clock, the clip progress 0–1; with a clock, the
age of each live event over its duration, 0–1. Each head then gets its own
clock: τ = (p − delay) / length. If phase is not 0, τ = (τ + phase) mod 1.

- **delay** does not wrap. Before the delay τ is below 0; after the head's
  length τ is above 1. A curve holds its first value below 0 and its last
  value above 1. Use delay for one-shots: a wipe, a cut, a dissolve, a bloom.
- **length** is how long the head's clock takes to go from 0 to 1. A curve
  over space on length gives each head its own speed.
- **phase** wraps, so the clock loops. Use phase for loops: a chase, a wave.

Put a curve over space into delay, length or phase to make heads differ.
`time(k, delay=curve(space(), "Ramp up"))` starts each head later along
the axis. The `low` and `high` of that curve set the spread in turns.

**space** `space(heads=None, direction=None, shift=0, length=1, kind="line", wrap=None)` → coordinate

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| heads | heads | | | all clip heads |
| direction | vector | uvz | not zero | best fit |
| shift | number | share | any | 0 |
| length | number | share | 0 or more (0 is a jump) | 1 |

The place of each head `a`, 0–1, then `x = (a − shift) / length`. With
`wrap`, `a − shift` wraps to 0–1 first. `shift` slides the place (the
shader's UV offset): a curve over time on shift moves the shape along the
heads, with the ease of that time curve. `length` is how much of the axis
reads as 0–1. There is no band: past 0 and 1 a curve holds its end values,
so give a moving pill jumps at its ends, `[[0, 0], [0, 1], [1, 1], [1, 0]]`.

Setting `kind`:
- `line`: the position along `direction`, lowest head 0, highest 1. Empty
  direction = the main axis of the heads.
- `order`: the rank of the head, `(rank + 0.5) / n`. After `shuffle` it is
  the shuffled rank.
- `radial`: distance from the centre, 0 at the centre, 1 at the edge.
  `direction` is the plane normal.
- `angle`: turns 0–1 around the centre.

Setting `wrap`: `True` or `False`. Empty = `False`, except `angle` (`True`).

A static region is a curve over space with a jump:
`curve(space(), [[0, 1], [0.5, 1], [0.5, 0], [1, 0]])` lights the first half.
A moving region is a curve over the space whose `shift` is a curve over
time: `move = curve(time(k), "Ramp up", low=-0.2, high=1)` then
`curve(space(shift=move, length=0.2), [[0, 0], [0, 1], [1, 1], [1, 0]])`
enters at one end and leaves at the other. To run the other way, use
"Ramp down" on the move, or the reverse `direction`.

A wrapped axis (`wrap=True`, and `angle`) is a ring: its ends do not meet,
so a phase curve over it loops with no seam.

**noise** `noise(heads=None, speed=None, scale=None, contrast=None)` → coordinate

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| heads | heads | | | all clip heads |
| speed | number | beats | above 0 | 4 |
| scale | number | share | above 0 | one value for all heads |
| contrast | number or list | share | 0–1 | 0 |

Smooth noise 0–1 over head position and time. `scale` is a share of the
rig's largest extent: 0.5 gives slow clouds, 0.02 gives each head its own
wander. Each noise node has its own stream.

**audio** `audio(low_hz=None, high_hz=None)` → coordinate

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| low_hz | number | hz | 20–20000 | 40 |
| high_hz | number | hz | above low_hz | 100 |

The energy of the band in the full mix, scaled 0–1 by its lowest and highest
value in the clip. `audio("Kick")` takes a band preset. Put a threshold or a
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

## Shapers

**mirror** `mirror(heads=None, normal=None, offset=None)` → heads

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| heads | heads | | | all clip heads |
| normal | vector | uvz | not zero | best fit |
| offset | number | metres | any | 0 |

Folds the heads across a plane through the middle of the rig. Order does not
change. Aim yaw and pitch mirror for folded heads. Two mirrors give four-fold
symmetry.

**shuffle** `shuffle(heads=None, clock=None)` → heads

A random order of the heads, from the clip seed. With a clock, a new order
per event. Positions do not change. Read it with `space(kind="order")`.
`curve(space(shuffle(clock=k), kind="order"), [[0, 1], [0.3, 1], [0.3, 0], [1, 0]])`
lights a random 30% of the heads per event.

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

`group.size`, `mirror.normal`, `mirror.offset`, `space.direction` and
`audio.*_hz` can change over time, but not across heads.

## Two clocks

The inputs of one node carry at most one clock, except the output node. So
`color(color=<clock A>, brightness=<clock B>)` is right, but a curve whose
`x` follows clock A and whose `low` follows clock B is an error.

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
- `color1.brightness: expected list items a share between 0 and 1; got 2. Example: brightness=[curve1, 0.5]`
- `curve1.gradient: expected a gradient because kind is color; got nothing. Example: gradient="Rainbow"`
- `clock1.every: expected beats above 0; got 0. Example: every=1, or leave the clock out for once over the clip`
- `graph: expected one output node; got color1 and strobe1. Example: one clip per output`
- `clip: expected a name; got none. Example: name="Kick chase"`

Do the example, then add the clip again.
