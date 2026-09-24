# Composable patterns

Status: built. Code: `backend/crates/patterns`, `backend/src/services/composable_patterns.rs`,
`backend/python/luma_exec/score.py`.

## Product contract

A node has typed inputs and outputs, and its body is either a fundamental native
operation or a graph. A score owns its custom graph definitions and clips. A
clip references a graph and carries timing, selection, z-order, blend mode,
seed, and input overrides. There is no separate Pattern/Implementation record
in the destination model. Graph labels are optional.

The shipped node library is fixed. A clip may start with one node exposing its
inputs. Repeated clips share the score-local definition; Make independent copies
its reachable local subgraphs. Built-in definitions remain shared and read-only.
There is no account-level custom library.

There is one input definition: type, default, evaluation rate, and description.
A binding is a literal, another node's output, or an enclosing graph input.
Exposure is a binding, not a synthetic Inputs node. An exposed input retains its
type and rich editor through nesting. Unsupported modulation is an error, never
a silently sampled constant. A playable graph exposes exactly one fixture-output
bundle; intermediate graphs remain composable nodes.

## Evaluation contract

Evaluation is seek-safe: a frame is a pure function of graph, clip inputs,
resolved venue cells, musical time, immutable analyzed track data, and instance
seed. Runtime cell IDs are not authored fixture references. Score selections
continue to name venue groups. All masks operate on independently controllable
cells/heads, including bar pixels.

Mapping assigns each selected cell a coordinate and domain. Open mappings may
clip or wrap; closed mappings wrap by default. Mapping owns grouping,
normalization, origin, and orientation. It does not own motion or stroke width.
Major axes are computed within each requested group, not by squashing global V.
Solved circles supply angular coordinates without re-normalizing their sampled
minimum and maximum (which would distort the closing segment).

Rhythm emits cycle progress from musical time. Clip alignment starts at clip
origin; grid alignment uses the track's musical origin. Motion returns position
and activity. Travel covers the complete authored journey. The remainder of a
repeat interval is dark; intervals shorter than travel are rejected until an
explicit overlapping-strokes operator exists. There is no accidental held-end
light during rest. Rate automation needs integrated phase and is not implemented
by multiplying absolute time by instantaneous rate.

Shape maps position to per-cell coverage. Width is a fraction of the mapped
domain. Clip suppresses outside coverage; wrap computes periodic distance, so a
pill straddles the seam. Outside-entry endpoints include half-width. Appearance
turns coverage and color into Lighting. Lighting composition is explicit.

Dissolve assigns each head a stable pseudorandom threshold. Coverage 1 lights
all heads; coverage 0 lights none. Softness controls each head's transition.
An optional beat interval refreshes the random order for percentage flicker.
Seeds depend on stable head identity, clip seed, and the stroke/refresh epoch,
never selection iteration order or frame count. A flash obtains its coverage by
sampling the shared Envelope type over its temporal phase.

Envelope is a reusable curve value. An envelope evaluator creates a temporal
signal; Chase samples it over the signed position within its width. Its native
per-clip editor offers presets and editable knots, and is not tied to a
specific node. Time durations have beat units; spatial proportions and
normalized positions have distinct types.

Gradient is the color equivalent of Envelope: the same ordered stops may be
sampled along clip progress or a mapped per-head coordinate. Colors use
normalized sRGB channels, and Gradient interpolates perceptually in OKLab,
matching the existing library and the native editor. Masks multiply color
before output separates chromaticity and dimmer. A dimmer-only output preserves
underlying color. Noise is a deterministic function of spatial coordinates,
musical time and the clip seed.

Track data is a read-only source, bound once during score preparation. Frequency
energy, drum-event time and harmony are fundamental sources; their response
curves and complete effects are ordinary graphs. Selecting an unavailable stem
or analysis is a preparation error. Audio source and drum choices remain typed
inputs, with the same choices in Python, graph controls and clip controls.

## Persistence

A score is rows. See [docs/design/sync.md](../design/sync.md).

- `scores` holds the score. `clips` holds one row per clip, with `graph`
  naming a built-in or score-local definition. `score_definitions` holds one
  row per score-local definition.
- `luma_patterns::Score` (version 7) is the in-memory type. Loading reads the
  three tables into a `Score`.
- Saving compares the candidate with the rows and writes only the rows that
  changed. There is no revision token. A stale candidate overwrites the rows it
  touches.
- The server records every insert, update and delete on an authored table in
  its history.
- PowerSync replicates the rows. Concurrent edits to one row merge per column;
  the last write wins.
- Undo in the editors stays in memory.
- A subagent edits a `drafts` row, not the live score. Merge writes the
  changed clips and definitions onto the live rows.

## Python

Discovery uses `luma.track.nodes(search)` and `definition(id)`. Editing uses
`edit.graph()`, `graph.node(definition_id, **inputs)`, output references,
exposed inputs and explicit graph outputs. `edit.graph(node="chase")` is the
one-node shortcut. `edit.add_clip(graph, beats=(32, 48), inputs={"width": .4})`
places it; `edit.make_independent(clip)` detaches local dependencies. A
subagent uses the same API on its draft.

Saving validates fixed input relationships without venue geometry. Host checks
also prepare the actual selected domain. Resource bounds in
`backend/crates/patterns/src/graph.rs` reject excessive nesting and expansion
before recursive execution: definition depth 24, 128 nodes per graph, 8192
expanded nodes, execution depth 192. A dynamic expression may still fail at
another sampled time.

## Acceptance path

- Place a one-node Chase graph from the native score's searchable picker.
- Override mapping, width, travel, repetition, shape and color per clip.
- Combine Chase Mask and Dissolve Mask within a clip and return fixture output.
- Exercise pixel bars, angled wings, circles, outside entry, wrapping and rest.
- Save/reopen, reuse a graph in another clip, then Make independent and edit it.
- Use the same definitions and edits through Python and a subagent draft.

## Known limits

- Continuous rate automation needs integrated phase.
- Per-group mapping of a union selection needs explicit mapping domains.
- The prepared evaluator flattens graph calls and folds constant expressions.
  It does not fuse ops.
