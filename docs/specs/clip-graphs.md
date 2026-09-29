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

## 1. Grammar in 10 lines

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

Promotion means: an input changes from a value to a wire. Every input is a
tensor over (heads, time, channels). A value has no heads or time axis and is
broadcast. A wire adds time, heads, or both.

---

## 2. Node reference

### 2.1 Types and units

Values:

| Type | JSON | Notes |
|---|---|---|
| number | `0.5` | Unit comes from the input. |
| vector | `[u, v, z]` | Stage frame: U right, V downstage, Z up. Direction or metres. |
| color | `[r, g, b]` | Linear Rec. 2020, each 0–1. |
| points | `{"points": [[x, v, ease], ...]}` | The one curve format. `x` 0→1 strictly increasing, `v` 0–1, 2–256 points, last point has no ease. |
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
| brightness | number | share | 0–1 | yes | 1 |
| alpha | number | share | 0–1 | yes | 1 |

Light per head = color × brightness × alpha. Blend modes: the light set
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
| phase | number | turns | 0–1 | yes | 0 |

Output: with no clock, `(beat − start) / clip duration`, clamped 0–1. With a
clock, the event's age over its duration, 0–1, one value per live event (axis
`E`). Phase adds and wraps: `(p + phase) mod 1` when phase ≠ 0. A wire on
phase over heads makes a spatial wave.

**space** → coordinate

| Input | Type | Unit | Range | Promotable | Empty |
|---|---|---|---|---|---|
| heads | heads | | | wire only | all clip heads |
| direction | vector | uvz | not zero | yes (T) | best fit |
| offset | number | share | any | yes | 0 |
| width | number | share | 0–4 | yes | 1 (whole axis) |

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

The stroke. `offset` is the start of the stroke and `width` its length, both
as a share of the axis:

```
d = a − offset                      (wrap no)
d = (a − offset) mod 1               (wrap yes)
x = d / width
inside = width > 0 and 0 ≤ x ≤ 1
```

`x` is the output: 0 at the start of the stroke, 1 at its end. A head that is
not inside is *outside*; the curve decides what outside means (2.4). With the
empty width (1) and offset 0, `x = a`: the whole axis. A pill 0.2 wide that
must enter and leave fully takes an offset curve from −0.2 to 1. There is no
overrun heuristic and no auto-reverse of the shape: the author sets low/high
and picks the shape's direction.

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

Output at `x` inside: `v = shape(x)`, then `low + v × (high − low)` (number
and vector, per component) or `gradient(v)` (color). Outside (a `space` head
not in the stroke): a number or vector curve gives `low`; a color curve gives
black. `x` from `time`, `noise` and `audio` is always inside.

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
   "offset":    {"type": "number", "unit": "share", "default": null, "range": null},
   "width":     {"type": "number", "unit": "share", "default": null, "range": [0, 4]}},
 "settings": {
   "kind": {"options": ["line", "order", "radial", "angle"], "default": "line"},
   "wrap": {"options": ["no", "yes"], "default": "no"}}}
```

`default: null` means empty. `Python: space(**{k: v["default"] for k, v in
definition("space")["inputs"].items()})` must pass the checker for every kind.

---

## 3. Type checker

One checker, in Rust (`clip_graph::check`). Score writes, the UI and Python
all run it. Python does not check on its own.

### 3.1 Rules, in order

1. Graph: `version` is 1; 1–64 nodes; ids match `[a-z]+[0-9]+`; exactly one
   output node; every other node reaches the output through wires; no cycle.
2. Node: known kind; only its inputs and settings; each setting is one of its
   options.
3. Value: right type for the input (number, vector, color, points, gradient);
   finite; in range; points valid (curve rules); gradient valid; a direction
   not zero.
4. Wire: right wire type; a curve's kind fits its destination; a curve that
   feeds two inputs has one unit; a curve's low/high in the destination's range
   (interval: `[min(low, high), max(low, high)]`, with wire bounds from their
   own curves).
5. Axes: an input that refuses an axis gets none of it (`group.size`,
   `mirror.offset`, `audio.*_hz`, `space.direction`, `mirror.normal` refuse
   `H`). The inputs of one node together carry at most one clock `E(k)`,
   except the output node, where each input may carry its own clock.
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
- `curve3: expected one unit; it feeds aim1.yaw (degrees) and space1.width (share). Example: make two curves`
- `curve1.gradient: expected a gradient because kind is color; got nothing. Example: gradient="Rainbow"`
- `space1.width: expected a share between 0 and 4; got a curve with low -1. Example: width=curve(t, "Ramp up", low=0, high=0.5)`
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
    "version": 1,
    "nodes": {
      "clock1": {"kind": "clock", "inputs": {"every": 2}},
      "time1":  {"kind": "time",  "inputs": {"clock": {"node": "clock1"}}},
      "curve1": {"kind": "curve", "settings": {"kind": "number"},
                 "inputs": {"x": {"node": "time1"}, "shape": {"points": [[0, 0], [1, 1]]}, "low": -0.2, "high": 1}},
      "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                 "inputs": {"offset": {"node": "curve1"}, "width": 0.2}},
      "curve2": {"kind": "curve", "settings": {"kind": "number"},
                 "inputs": {"x": {"node": "space1"}, "shape": {"points": [[0, 1], [1, 1]]}}},
      "color1": {"kind": "color", "inputs": {"color": [1, 1, 1], "brightness": {"node": "curve2"}}}}}}}}
```

Rules: an empty input is absent. A wire is `{"node": id}`. Settings at their
default are still written. `selection_seed` stays as it is. The Rust type:

```rust
pub struct Clip { pub name: String, pub start: f64, pub duration: f64, pub seed: u64,
    pub selection_seed: Option<u64>, pub selection: Selection, pub z_index: i64,
    pub blend_mode: BlendMode, pub graph: ClipGraph }
pub struct ClipGraph { pub version: u32, pub nodes: BTreeMap<String, Node> }
pub struct Node { pub kind: Kind, pub settings: BTreeMap<String, String>, pub inputs: BTreeMap<String, Input> }
pub enum Input { Number(f64), Vector([f64; 3]), Color([f64; 3]), Points(Curve<f64>), Gradient(Gradient), Wire(String) }
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
3. `time`: `ClipProgress`, or the clock's `progress`, then `Shift(phase)`.
4. `space`: constant `a` field (or `Axis`), then `Stroke(a, offset, width, wrap)`
   → `x, inside`.
5. `noise`: `ClockTable(speed)` → `Clock` turns → `Noise4(u,v,z, turns, scale, contrast, salt)`.
6. `audio`: `band_energy` → `clip_range` → `Normalize` → 0–1.
7. `curve`: `Curve(x, inside, low, high; shape, gradient, kind)` → 3 components.
8. `shuffle` with a clock: `Rank(index, unit keys)` per event → order field with `E`.
9. Output: per clock group, `Pick(present, index, effect factors…)` → winner,
   `Gather(value, winner)` per input; then `ColorOut`, `StrobeOut` or
   `AimOut` (the `SourceOp::Aim` math without lean) → `output` terminal,
   plus the `turn` output for aim.

Kernel list (`clip_graph::kernels`, replacing `forms::source_ops` and
`forms::ops`): `ClockTable`, `Clock`, `Events`, `ClipProgress`, `Shift`,
`Axis`, `Fold`, `Stroke`, `Noise4`, `Normalize`, `Curve`, `Rank`, `Pick`,
`Gather`, `ColorOut`, `StrobeOut`, `AimOut`. Plus the kept `BandEnergy`,
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
time(clock=None, phase=0) -> Coordinate
space(heads=None, direction=None, offset=None, width=None, kind="line", wrap=None) -> Coordinate
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
→ vector; else number. `wrap=None` takes the kind's default.

Variable reuse is linking: the same Python object wired twice is one node.
Ids are `<kind><n>` in creation order per kind (`clock1`, `curve3`); the
same ids appear in the UI and in errors. Python does not type check; it only
refuses unknown keywords.

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
pos = curve(time(k), "Ramp up", low=-0.2, high=1)
edit.add_clip(color(brightness=curve(space(offset=pos, width=0.2), "On")),
              name="Chase", beats=(32, 48), selection="bars")
```

**Sparkle (Shimmer)**

Before: a `random` brightness with `events {every 0.125, life 0.5}`,
`coverage 0.3`, `level` a Time spike curve.
After:
```python
k = clock(every=0.125, duration=0.5)
lit = curve(space(shuffle(clock=k), kind="order", offset=0, width=0.3), "On")
edit.add_clip(color(brightness=lit, alpha=curve(time(k), "Spike")),
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

A node card: title row with the kind in sentence case plus its number
("Curve 2"), its settings as `float::segmented()` controls
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

The new `space` takes an empty direction and width 0.2 with `offset` already
wired `Over time` (Ramp up, −0.2 → 1) so "over space" moves; the new `clock`
takes every 1.

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
| Wash | `color(color=(1,1,1))` | color |
| Pulse | `color(brightness=curve(time(clock(every=1)), "Drop"))` | clock → time → curve → brightness |
| Breathe | `color(brightness=curve(time(clock(every=4)), "Swell"))` | clock → time → curve → brightness |
| Fade | `color(alpha=curve(time(), "Fade in"))` | time → curve → alpha |
| Color fade | `color(color=curve(time(), "Ramp up", gradient=[(0,"#b0400a"),(1,"#2449eb")]))` | time → curve(color) → color |
| Rainbow | `color(color=curve(time(clock(every=4)), "Ramp up", gradient="Rainbow"))` | clock → time → curve(color) |
| Gradient | `color(color=curve(space(), "Ramp up", gradient="Sunset"))` | space → curve(color) |
| Stepped palette | `color(color=curve(time(clock(every=4)), "Steps 4", gradient="Rainbow"))` | clock → time → curve(color) |
| Two-color swap | `color(color=curve(time(clock(every=2)), "Square", gradient=[(0,"#ff2a00"),(1,"#0040ff")]))` | clock → time → curve(color) |
| Follows a band | `color(brightness=curve(audio(40, 100), "Ramp up"))` | audio → curve → brightness |
| VU meter | `color(brightness=curve(space(split(), direction=(0,0,1), offset=0, width=curve(audio(20,250), "Ramp up")), "On"))` | audio → curve → space.width; split → space → curve → brightness |
| Random heads | `k=clock(every=1); color(brightness=curve(space(shuffle(clock=k), kind="order", offset=0, width=0.5), "On"))` | clock → shuffle → space(order) → curve |
| Random bars | `k=clock(every=1); color(brightness=curve(space(shuffle(group(), k), kind="order", offset=0, width=0.5), "On"))` | clock → shuffle(group) → space → curve |
| Sparkle | `k=clock(every=0.125, duration=0.5); color(brightness=curve(space(shuffle(clock=k), kind="order", offset=0, width=0.3), "On"), alpha=curve(time(k), "Spike"))` | one clock → shuffle → space → curve → brightness; time → curve → alpha |
| Build | `color(brightness=curve(space(shuffle(), kind="order", offset=0, width=curve(time(), "Ramp up")), "On"))` | time → curve → space.width; shuffle → space → curve |
| Dissolve | as Build with `"Ramp down"` | same |
| Clouds | `n=noise(speed=8, scale=0.5); color(color=curve(n, "Ramp up", gradient="Ocean"), brightness=curve(n, "Ramp up", low=0.2, high=1))` | one noise → two curves |
| Sparkle rain | `k=clock(every=0.25, duration=1); fall=curve(space(split(), direction=(0,0,-1), offset=curve(time(k), "Ramp up", low=-0.3, high=1), width=0.3), "Comet"); color(brightness=fall, alpha=curve(space(shuffle(group(), k), kind="order", offset=0, width=0.2), "On"))` | needs vertical bars; one clock drives the fall and the per-bar pick |

### Movement

| Name | Python | Chain |
|---|---|---|
| Chase | `k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=0.2), "On"))` | clock → time → curve → space.offset; space → curve |
| Wave | `k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-1, high=1), width=1), "Soft"))` | same, wide soft stroke |
| Bounce | `k=clock(every=4); color(brightness=curve(space(offset=curve(time(k), "Triangle", low=0, high=0.8), width=0.2), "Soft"))` | offset there and back |
| Comet | Chase with `"Comet"` | same |
| Wipe | `k=clock(every=4); color(brightness=curve(space(offset=0, width=curve(time(k), "Ramp up")), "On"))` | time → curve → space.width |
| Stepped chase | `k=clock(every=4); color(brightness=curve(space(offset=curve(time(k), "Steps 4", low=0, high=0.75), width=0.25), "On"))` | held offsets |
| Many pills | `k=clock(every=0.5, duration=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=0.2), "On"))` | four events alive at once |
| Wrapping chase | `k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up"), width=0.2, wrap=True), "On"))` | wrap on a line |
| Colored pills | `k=clock(every=0.5, duration=2); s=space(offset=curve(time(k), "Ramp up", low=-0.25, high=1), width=0.25); color(color=curve(time(k), "Ramp up", gradient="Rainbow"), brightness=curve(s, "Soft"))` | each event has its own progress, so its own color |
| Speed-up chase | `t=time(); k=clock(every=curve(t, "Ramp down", low=0.25, high=2), duration=curve(t, "Ramp down", low=0.5, high=2)); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.4, high=1), width=curve(t, "Ramp down", low=0.1, high=0.4)), "Comet"))` | one time() feeds every, duration and width; events multiply as they shrink |
| Alternating sides | `k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Square", low=0, high=0.5), width=0.5), "On"))` | half the axis, swapping |
| Diagonal slash | `k=clock(every=2); color(brightness=curve(space(direction=(1,0,1), offset=curve(time(k), "Ramp up", low=-0.15, high=1), width=0.15), "On"))` | a diagonal line direction |
| Ripple | `k=clock(every=2); color(brightness=curve(space(kind="radial", offset=curve(time(k), "Ramp up", low=-0.4, high=1), width=0.4), "Soft"))` | radial stroke |
| Wrapping ripple | Ripple with `wrap=True` | rings re-enter at the centre |
| Spin | `k=clock(every=2); color(brightness=curve(space(kind="angle", offset=curve(time(k), "Ramp up"), width=0.25), "Comet"))` | angle wraps by default |
| Grow | `color(brightness=curve(space(kind="radial", offset=0, width=curve(time(), "Ramp up")), "On"))` | width grows from the centre |
| Turning line | `k=clock(every=2, duration=4); color(brightness=curve(space(kind="angle", offset=curve(time(k), "Ramp up"), width=0.1), "On"))` | duration = 2 × every: two opposite arms alive |
| Spiral | `k=clock(every=4); t=time(k, phase=curve(space(kind="radial"), "Ramp up")); color(brightness=curve(space(kind="angle", offset=curve(t, "Ramp up"), width=0.3), "Soft"))` | phase by radius bends the arm |
| Mirror | `m=mirror(); k=clock(every=2); color(brightness=curve(space(m, offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=0.2), "On"))` | pills from both ends meet |
| Kaleidoscope | `m=mirror(mirror(normal=(1,0,0)), normal=(0,0,1)); k=clock(every=4); color(brightness=curve(space(m, kind="angle", offset=curve(time(k), "Ramp up"), width=0.15), "Comet"))` | two mirrors, four-fold |

### Aim

| Name | Python | Chain |
|---|---|---|
| Position | `aim(direction=D)` | aim |
| Fan | `aim(direction=D, yaw=curve(space(), "Ramp up", low=-25, high=25))` | space → curve → yaw |
| Converge | `aim(base="point", point=(0, 3, 0))` | point base |
| Follow | `aim(base="point", point=curve(time(), "Ramp up", low=(-3,3,0), high=(3,3,0)))` | time → curve(vector) → point |
| Bloom | `aim(base="away", point=curve(time(), "Ramp up", low=(0,0,40), high=(0,0,7)))` | away base; the point comes down toward the rig |
| Tunnel | `aim(base="point", point=(0, 25, 1.5))` | a far point downstage |
| Sweep | `aim(direction=D, yaw=curve(time(clock(every=8)), "Sine", low=-45, high=45))` | clock → time → curve → yaw |
| Nod wave | `k=clock(every=4); aim(direction=D, pitch=curve(time(k, phase=curve(space(), "Ramp up", high=0.6)), "Sine", low=-25, high=25))` | space → curve → time.phase |
| Circle | `t=time(clock(every=4)); aim(direction=D, yaw=curve(t, "Cosine", low=-18, high=18), pitch=curve(t, "Sine", low=-18, high=18))` | one time → two curves |
| Figure-8 | `t=time(clock(every=4)); aim(direction=D, yaw=curve(t, "Sine", low=-25, high=25), pitch=curve(t, "Double sine", low=-12.5, high=12.5))` | same, doubled pitch |
| Pinwheel | `t=time(clock(every=4), phase=curve(space(kind="angle"), "Ramp up")); aim(direction=D, yaw=curve(t, "Cosine", low=-20, high=20), pitch=curve(t, "Sine", low=-20, high=20))` | Circle with phase by angle |
| Scissor | `m=mirror(); aim(heads=m, direction=D, yaw=curve(time(clock(every=4)), "Sine", low=-30, high=30))` | mirrored heads yaw the other way |
| Up/down flip | `aim(direction=D, pitch=curve(time(clock(every=2)), "Square", low=-30, high=30))` | held pitch |
| Ballyhoo | `aim(direction=D, yaw=curve(noise(speed=4, scale=0.02), "Ramp up", low=-40, high=40), pitch=curve(noise(speed=4, scale=0.02), "Ramp up", low=-40, high=40))` | two noise nodes, two streams |

Motion presets ship with blend `offset` and a Position underneath is the
usual pairing.

### Strobe

| Name | Python | Chain |
|---|---|---|
| Strobe | `strobe(rate=0.9)` | strobe |
| Ramp | `strobe(rate=curve(time(), "Ramp up"))` | time → curve → rate |
| Strobe follows a band | `strobe(rate=curve(audio(40, 100), "Ramp up", low=0.3, high=1))` | audio → curve → rate |

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
