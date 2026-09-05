# Composable patterns

Status: implementation in progress on `codex/composable-patterns`.

## Product contract

A pattern is a playable composition containing a graph. A clip is a timed
instance of a pattern, with overrides of its exposed inputs. Patterns authored
from the score belong to that score unless explicitly saved to the library.
Reusable graphs may produce intermediate values; only graphs with a Lighting
output appear in the score insertion picker.

There is one input definition: type, default, evaluation rate, and description.
A binding is a literal, another node's output, or an enclosing graph input.
An exposed input retains its type and rich editor through arbitrary nesting.
No separate args/params storage in the new graph format. Connections are checked
before execution; unsupported modulation is an error, never a sampled constant.
Definitions are revision-addressed. Updating a library definition does not
silently change clips. Copying a definition locally permits score-only edits.

## Evaluation contract

Evaluation is seek-safe: a frame is a pure function of graph, clip inputs,
resolved venue cells, musical time, and instance seed. Runtime cell IDs are
not authored fixture references. Score selections continue to name venue groups.
All masks operate on independently controllable cells/heads, including bar pixels.

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

Dissolve assigns each cell a stable pseudorandom threshold for a stroke. Progress
removes coverage monotonically; softness controls each cell's transition. Seeds
are based on cell identity, instance seed, and optionally cycle identity, never
selection iteration order or frame count. Progress 0 is fully lit; 1 is dark.

Envelope is a reusable curve value. An envelope evaluator creates a temporal
signal; the editor is not tied to a specific node. Time durations have beat units;
spatial proportions and normalized positions have distinct types.

## Acceptance path

- Place a one-node Chase pattern directly from the score's searchable picker.
- Override its mapping, width, travel, repetition, and appearance per clip.
- Open the graph, compose Chase Mask with Dissolve Mask, return Lighting.
- Exercise pixel bars, angled wings, circles, outside entry, wrapping, and rest.
- Save/reopen a score; promote a local pattern to the library and copy it back.
- Migrate an isolated copy of existing authored data before touching live data.

## Delivery state

The first slice establishes executable graph contracts and effect kernels.
Persistence, migration, score insertion, and rich input editors must consume those
contracts; a second disconnected runtime or an indefinitely retained legacy
editor is not the finished result.
