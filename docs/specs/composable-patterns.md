# Composable patterns

Status: built. Code: `backend/crates/patterns`, `backend/src/services/composable_patterns.rs`,
`backend/python/luma_exec/score.py`.

## Product contract

A score owns its clips. Each clip holds one small typed graph (a clip graph,
specified in [clip-graphs.md](clip-graphs.md)) and carries its name, timing,
selection, z-order, blend mode and seed. Graphs are not shared: a clip copied
from a preset or another clip is its own. The shipped node kinds are fixed,
and there is no account-level custom library.

## Evaluation contract

Evaluation is seek-safe: a frame is a pure function of the graph, the live
control values, resolved venue cells, musical time, immutable analyzed track
data, and the clip seed. The one exception is a clock whose period a live
control moves, kept between frames
([live-control.md](../design/live-control.md)). Runtime cell IDs are not authored fixture references. Score selections
continue to name venue groups. All masks operate on independently controllable
cells/heads, including bar pixels.

Envelope is a reusable curve value. Time durations have beat units; spatial
proportions and normalized positions have distinct types.

Gradient is the color equivalent of Envelope: ordered stops sampled by any
0–1 value, such as clip progress or a head's place in space. Colors are
linear Rec. 2020, 0–1 per channel, and Gradient interpolates perceptually in
OKLab, matching the native editor. Fixtures and the visualizer map a color
their emitters cannot make to the nearest one they can
(`backend/crates/patterns/src/color_space.rs`). Output separates chromaticity
and dimmer. Random is a deterministic function of head identity, musical time
and the clip seed.

Track data is a read-only source, bound once during score preparation. Band
energy of the full mix is the fundamental audio source; response curves and
complete effects are ordinary graphs. Missing analysis is a preparation error.

## Persistence

A score is rows. See [docs/design/sync.md](../design/sync.md).

- `scores` holds the score. `clips` holds one row per clip, its graph in
  `graph_json` (`backend/src/database/local/scores/rows.rs`).
- `luma_patterns::Score` is the in-memory type: the clips by id.
- Saving compares the candidate with the rows and writes only the rows that
  changed. There is no revision token. A stale candidate overwrites the rows it
  touches.
- The server records every insert, update and delete on an authored table in
  its history.
- PowerSync replicates the rows. Concurrent edits to one row merge per column;
  the last write wins.
- Undo in the editors stays in memory.
- A subagent edits a `drafts` row, not the live score. Merge writes the
  changed clips onto the live rows.

## Python

Python builds clip graphs with `luma.clip`'s builders and places them with
`edit.add_clip(graph, name=..., bars=(17, 21))` (or `at=`, `seconds=`) on
`luma.track.edit()`; `check()` and `apply()` run Rust's checker and write the
rows. See [agent-code-execution.md](../design/agent-code-execution.md) §18 and
[clip-graphs.md](clip-graphs.md) §6. A subagent uses the same API on its draft.

Saving checks a graph without venue geometry. Host checks also prepare the
actual selected heads. A dynamic expression may still fail at another sampled
time.

## Known limits

- The prepared evaluator folds constant expressions. It does not fuse ops.
