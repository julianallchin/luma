# Graph editor

**Status:** built. The user agreed this behavior on 2026-09-09. Every score
graph opens in one editor, `gpui/crates/app/src/graph.rs`. The interaction
architecture is in [graph-editor-interaction.md](graph-editor-interaction.md).

## Signals and execution

- A numerical wire carries a fixture × time × channel signal. Scalars
  broadcast. Units and channel meaning are inferred. The editor checks them at
  the wire, and the runtime checks them again.
- Structured values stay structured: choices, mappings, envelopes and
  gradients.
- One tensor runtime executes every graph (`backend/crates/patterns/src/runtime`,
  `prepared.rs`, `tensor.rs`). A host evaluates a whole time batch in one call.
  There is no per-sample graph run and no playback state.
- Envelope is the one editable curve (`envelope.rs`). Saved ADSR and Beat
  Envelope nodes convert to Envelope when a graph loads.
- Seeds are u64 values. They are stored as decimal strings so JSON readers
  cannot round them.

## Chase and events

- One Chase accepts reusable triggers. With no trigger wired, it uses its
  periodic repeat, grid and delay. Beat and drum triggers can drive Chase,
  Pulse and other effects.
- Each event starts an independent journey. Travel time and curve do not
  depend on event spacing. Equal travel and spacing gives back-to-back
  journeys. Longer travel overlaps. Shorter travel leaves darkness.
- A later event never cuts an earlier journey. Overlapping journeys combine
  with Max at each fixture.
- Execution is tensor algebra (`event_tensor.rs`). Sample times broadcast
  against event timestamps to give ages. Ages broadcast against fixture
  coordinates. Max reduces the event axis. There is no mutable journey state
  and no per-event replay, so a seek gives the same result as playback.
- An event can carry head weights or a random subset of heads
  (`event_targets.rs`). The subset depends on event identity, seed and head
  identity only.

## Mapping

- Mapping presets: axis, order, major axis and circle. A custom UVZ vector is
  also available. Vector length does not change the mapping or the travel
  time.
- A mirror plane folds positions before mapping (`mapping.rs`, `MirrorPlane`).
  The plane does not depend on travel direction. Presets are Off, left/right,
  front/back, up/down and a custom normal. The plane offset is measured from
  the selection centre.

## Inputs, cards and Output

- An Input node has a name and one output socket. Its first wire gives it the
  destination's type, editor metadata and default. More consumers share the
  value and must be compatible.
- Renaming an Input changes its label only. Its key stays the same, so clip
  overrides and references keep working.
- Cards show names and sockets only. The inspector edits values.
- Output is a terminal node with optional color, dimmer, pan, tilt, strobe
  and speed inputs. An unwired capability is not written. An explicit zero is
  a write.

## Editing

- Drag from a socket to wire. Valid targets highlight. An invalid target
  shows the reason. A failed drop keeps existing wires.
- Right-click or Space opens node search at the pointer. Enter inserts the
  node at that point.
- Edges can be selected and deleted. Undo and redo cover every gesture.
- The toolbar has `100%` and `Fit`. The shell has Expand / Show chat.
- Search lists built-in nodes and score-local graphs.

## Preview

- The visualizer plays the selected clip with its real timing, selection,
  seed and overrides. Play, pause, loop and scrub are temporary. They do not
  write the score.
- An evaluation error names the failing node and stops the audio preview.
- Inspect signals (`gpui/crates/app/src/graph/preview/inspection.rs`) plots
  extra numerical and event outputs, and spectrograms.

## Saved data

Older score documents convert to the current version when a score loads
(`backend/crates/patterns/src/migration.rs`).

## Known limits

- Per-group mapping treats a union selection as one group. It cannot split the
  union into its member groups.
- Undo of a node deletion restores the nodes and wires, but not the selection.
- Conversion parity has known differences. The reference captures use a
  constant tempo. Negative old chromaticity is clamped at Output. Avoid Repeat
  becomes a shuffled cycle, so exact head sequences change. A zero-length
  envelope is silent.
- Laser output is not built. It needs an ordered scan-point domain and a
  device adapter.
