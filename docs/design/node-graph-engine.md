# Node Graph Engine

The pattern graph engine is the crate `luma-patterns` in
`backend/crates/patterns`. It has no database, UI or playback-device state. It
evaluates graphs in musical time over a list of cells. The backend supplies the
cells, the track data and the times.

## Scores

A score is the document that one editor edits. The type is
`luma_patterns::Score` in `score.rs`. It holds `clips` on the track timeline,
keyed by id. A score has at most 2,048 clips.

A `Clip` has these fields:

| Field | Meaning |
|-------|---------|
| `name` | The clip's name, at most 64 characters |
| `start` | Start position, in beats |
| `duration` | Length, in beats |
| `seed` | Seed for random choices |
| `selection` | The whole venue, or a union of its groups by id |
| `z_index` | Layer order |
| `blend_mode` | How the clip combines with the layers below it |
| `graph` | The clip's own `ClipGraph` |

In the database a score is rows in `scores` and `clips`.
See [Database](database.md).

## Clip graphs

A `ClipGraph` (in `clip_graph/mod.rs`) is the graph a clip owns: a version
(3) and up to 64 nodes keyed by id. Each node has a kind, its settings and its
inputs. An input holds a value, a wire (`{"node": id}`) or nothing. Exactly
one node is the output: `color`, `aim`, `strobe` or `laser`. The spec is
`docs/specs/clip-graphs.md`.

`clip_graph::definitions()` (in `clip_graph/definitions.rs`) gives one
definition per kind: its inputs with type, unit and range, its settings with
their options, and what its output wire carries. The editor, the Python
builders and the checker all read them. The
[Node Reference](https://luma.show/docs/node-reference) is generated from them.

`clip_graph::check` checks a graph in five steps: the graph (version, node
count, ids, one output, no loop, every node feeding the output), each node's
inputs and settings, each value, each wire's type, and the axes a wire may
carry. `check_clip` also checks the clip's name, its blend mode for its
output kind, and its start and duration. `Score::validate_clip` runs both,
then lowers the graph to catch fixed timing and values that fail.

## Kernel graphs

Before a clip plays, `clip_graph::lower` turns its graph into a kernel graph.
A `Definition` (in `graph.rs`) has a name, named inputs, named outputs and a
body. The body is one of two kinds:

- **Primitive**: a Rust kernel.
- **Graph**: nodes, graph inputs and output bindings.

Each node names a definition. Each node input has a `Binding`:

- `value`: a constant.
- `input`: a graph input of the enclosing definition.
- `connection`: an output of another node.

Each `Input` has a name, a description, a value type, a rate and an optional
default. The rate is `fixed` or `frame`. A `frame` input can change on each
frame. A `fixed` input cannot.

`standard_library()` in `catalog.rs` builds the kernel library: every
`clip_graph::Kernel`, plus `band_energy`, `control` and the `output` terminal.
Users and agents never edit kernel graphs.

## Signals

Numerical values are `luma_patterns::Signal` values (in `tensor.rs`). A signal
has four parts:

| Part | Meaning |
|------|---------|
| `values` | A 3D `f64` array: fixture × time × channel |
| `unit` | `number`, `beats`, `proportion`, `position`, `degrees` or `seconds` |
| `channels` | `value`, `rgb`, `pan_tilt` or `components(n)` |
| `fixtures` | An ordered list of fixture ids, or none |

A signal with no fixture list applies to every fixture. A signal with a fixture
list can only combine with signals over the same fixtures. Equal sizes are not
enough.

A wire type can leave the unit or the channels unset. The engine then infers
them from the connected values. Two units are compatible when they are equal,
or when both are dimensionless (`number`, `proportion` or `position`).

Arithmetic checks units:

- Multiply by a `number` or `proportion` keeps the other unit.
- Divide two equal units gives a `number`.
- Add, subtract, minimum and maximum need equal units, or one `number`.

Other values are structured, not numerical: gradients, curve points, vectors
and lighting.

## Preparation

`PreparedGraph::new` (in `prepared.rs`) checks a clip graph, lowers it onto
the kernel library over the frame's cells, and flattens the result into a list
of steps. Fixed outputs are computed once. The frame loop does not walk
definitions and does not repeat geometry solves. A graph's faders and buttons
read a `ControlSource` bound with `PreparedGraph::with_controls`, so moving a
control never prepares the graph again.

## Musical time

Clip positions are in beats. `BeatTimeline` (in `clock.rs`) maps seconds to
beats and beats to seconds. It uses the detected beats of the track, so it
follows tempo changes. Outside the detected beats it extrapolates with the edge
interval.

Evaluation is a pure function of the requested time. Any time can be the first
time evaluated. Playback, scrubbing and batch rendering use the same code.
There is no fixed simulation rate.

## Output

Every lowered graph ends in the kernel `output` terminal. Its inputs are
`color`, `pan`, `tilt`, `strobe`, `speed`, `alpha` and `aim`, all optional.
The clip graph's output node binds only what it drives: color, strobe or
aim, and the clip's alpha. An unbound input leaves that capability to lower layers. This is
different from writing zero. A `laser` node draws laser elements instead.

There is no dimmer input. The dimmer is the peak channel of the color. The
color is normalized by that value (`FixtureOutput::from_rgb` in `output.rs`).

Every light color is linear Rec. 2020, 0–1 per channel. Gradients blend in
OKLab. A fixture or the visualizer converts the color to its own emitters;
`backend/crates/patterns/src/color_space.rs` holds the conversions and the
gamut mapping.

## Host side

- `backend/src/services/graph_scores.rs`: `prepare_scene` compiles a score into
  a `Scene` for the compositor. It uses the venue geometry, the group
  selections and the beat grid of one authorized read. It converts each clip
  span from beats to seconds with `BeatTimeline::seconds_at`.

See [Compositor](compositor.md) for how clips combine, and
[Selection System](selection-system.md) for how cells are
resolved.
