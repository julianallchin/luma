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
4. An input holds a value (number, vector, color, points, gradient), a wire, or nothing.
5. A choice (`kind`, `wrap`, `base`, `by`) is a setting on a node. It is never wired.
6. Coordinate nodes give a raw 0–1 coordinate: `clock`+`time`, `space`, `noise`, `audio`.
7. `curve` is the only node that turns a coordinate into a value: number, vector or color.
8. Shapers change the head set: `mirror`, `shuffle`, `group`, `split`. They stack.
9. An empty input is the only default: no clock = once over the clip; no heads = all heads; no direction = best fit; no width = whole axis; no size = one fixture.
10. Overlap is automatic. Per head, the event with the biggest effect shows; ties go to the newest.

Promotion means: an input changes from a value to a wire. Every number,
vector and color input can take a wire.

## Values and units

| Type | Python | Notes |
|---|---|---|
| number | `0.5` | The unit comes from the input. |
| vector | `(u, v, z)` | Stage frame: U right, V downstage, Z up. A direction or metres. |
| color | `(r, g, b)` or `"#RRGGBB"` | Linear Rec. 2020, each 0–1. Hex is sRGB and is converted. |
| points | `"Comet"`, `[[x, v], [x, v, ease], ...]` | A curve preset name or points. |
| gradient | `"Fire"`, `[(t, color), ...]` | A gradient preset name or stops. Blends in OKLab. |
| choice | `kind="order"` | A setting. |

Units: share (0–1), beats, degrees, metres, hz, turns (0–1), uvz (a vector), rgb.

Points: `x` goes from 0 to 1 and strictly increases. `v` is 0–1. A curve has
2–256 points. The ease says how the value moves to the next point: `linear`
(no ease), `hold`, `ease-in`, `ease-out`, `ease-in-out`, `sine-in`,
`sine-out`, `sine-in-out`, or a local cubic Bézier `[x1, y1, x2, y2]`. The
last point has no ease.

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
| brightness | number | share | 0–1 | 1 |
| alpha | number | share | 0–1 | 1 |

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

**time** `time(clock=None, phase=0)` → coordinate

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| clock | clock | | | once over the clip |
| phase | number | turns | 0–1 | 0 |

With no clock: clip progress 0–1. With a clock: the age of each live event
over its duration, 0–1. Phase adds and wraps. A curve over space on phase
makes a wave across the heads.

**space** `space(heads=None, direction=None, offset=None, width=None, kind="line", wrap=None)` → coordinate

| Input | Type | Unit | Range | Empty |
|---|---|---|---|---|
| heads | heads | | | all clip heads |
| direction | vector | uvz | not zero | best fit |
| offset | number | share | any | 0 |
| width | number | share | 0–4 | 1 (whole axis) |

Setting `kind`:
- `line`: the position along `direction`, lowest head 0, highest 1. Empty
  direction = the main axis of the heads.
- `order`: the rank of the head, `(rank + 0.5) / n`. After `shuffle` it is
  the shuffled rank.
- `radial`: distance from the centre, 0 at the centre, 1 at the edge.
  `direction` is the plane normal.
- `angle`: turns 0–1 around the centre.

Setting `wrap`: `True` or `False`. Empty = `False`, except `angle` (`True`).

The stroke: `offset` is where it starts and `width` is its length, both as a
share of the axis. The output is 0 at the start of the stroke and 1 at its
end. A head outside the stroke reads the curve's `low` (black for a color
curve). A pill 0.2 wide that enters and leaves fully needs an offset curve
from -0.2 to 1. There is no auto-reverse: to run the other way, use a
"Ramp down" offset and flip the shape yourself.

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
`space(shuffle(clock=k), kind="order", offset=0, width=0.3)` lights a random
30% of the heads per event.

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

Curves (`v` 0–1): On, Ramp up, Ramp down, Triangle, Soft, Comet, Spike,
Drop, Swell, Fade in, Fade out, Square, Sine, Cosine, Double sine, Steps 2,
Steps 3, Steps 4, Steps 8. Gradients: Rainbow, Warm, Cool, Fire, Ocean,
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
- `curve3: expected one unit; it feeds aim1.yaw (degrees) and space1.width (share). Example: make two curves`
- `curve1.gradient: expected a gradient because kind is color; got nothing. Example: gradient="Rainbow"`
- `clock1.every: expected beats above 0; got 0. Example: every=1, or leave the clock out for once over the clip`
- `graph: expected one output node; got color1 and strobe1. Example: one clip per output`
- `clip: expected a name; got none. Example: name="Kick chase"`

Do the example, then add the clip again.
