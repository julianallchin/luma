# Composable pattern contracts

This crate owns clip graphs (`src/clip_graph/`), the kernels they lower onto,
fixture × time × channel tensors and live controls. It has no database, UI or
playback-device state.

See [clip graphs](../../../docs/specs/clip-graphs.md) for the graph model, its
node kinds, schema and checks, and
[composable patterns](../../../docs/specs/composable-patterns.md) for the
evaluation and persistence contract.

- A clip graph has exactly one output node: `color`, `aim`, `strobe` or
  `laser`. `clip_graph::check` is the one type checker. A checked graph lowers
  onto the kernels in `clip_graph/kernels.rs`; `standard_library()` holds only
  those kernels, band energy, live controls and the output terminal. There is
  no separate form interpreter.
- `PreparedGraph::new(library, graph, frame)` flattens a graph once and
  executes each operation once per requested time batch. Static work folds
  once; clocks integrate into fixed tables, so evaluation has no playback
  history. Native visualizer previews share this path, including real track
  audio, beat-grid interpolation and argument overrides.
- Track seconds and musical beats have distinct units. There is one curve
  format (`Curve`) and one shared sampler. A curve is
  `{"points": [[x, value], [x, value, ease], ...]}` with x strictly increasing
  from 0 to 1. The ease moves the value from its point to the next:
  `"linear"` (the default, never written), `"ease-in"`, `"ease-out"`,
  `"ease-in-out"`, `"hold"` (jump at the next point) or a CSS `cubic-bezier`
  `[x1, y1, x2, y2]` local to the segment. The last point has no ease. An
  `Envelope` is a curve of values 0..1.
- `band_energy` reads the energy of a frequency band of the track's full mix.
  The host prepares the mix once; a missing analysis is an error.
- Every light color is linear Rec. 2020, 0–1 per channel, with no tag:
  stored colors, gradient stops, presets, the compositor and
  `FixtureOutput`. Brightness is the peak channel, split out as the dimmer.
  `src/color_space.rs` holds the conversions: OKLab for blending, sRGB hex for
  people, and `Gamut` for emitters. A color that a fixture's emitters or the
  display cannot make is mapped to the nearest one they can, in OKLab, by
  reducing chroma at constant lightness and hue. A fixture's red, green and
  blue are assumed to have sRGB primaries.
- Gradients interpolate perceptually in OKLab and keep stop opacity; absent
  opacity means one.
- `FixtureOutput` keeps an unwritten capability (color, dimmer, strobe,
  speed, aim) distinct from one written as zero. Aim is a direction; a solver
  turns it into pan and tilt after compositing.
- Timing, selection, layering and the random seed belong to the clip
  (`Clip`). Random draws are stable per head.
- Live controls (`src/controls.rs`) are the fader, live color and button
  nodes; a host binds them with `PreparedGraph::with_controls`, and with none
  bound each reads 0. `src/press.rs` says how a button plays: toggle or
  flash, an ADSR, and a start on the next beat or bar.
- `src/laser.rs` holds laser elements on a zone's canvas: figures of beams,
  lines, rings or polygons. See [lasers](../../../docs/design/lasers.md).
- `src/presets.json` holds the shipped presets: clip graphs named as the
  effect, curve shapes, gradients and frequency bands.

The host supplies selected cells and their world and U/V/Z coordinates.
A selection names groups by id (`src/selection.rs`); resolving it to cells is
the host's job.

Run checks from the repository root:

```
cargo test --manifest-path backend/Cargo.toml -p luma-patterns
cargo clippy --manifest-path backend/Cargo.toml -p luma-patterns --all-targets -- -D warnings
```

## Native venue previews

The shared native/headless dispatcher exposes `preview_composable_pattern`
(`backend/src/models/composable_patterns.rs`). Its `request` carries
`venueId`, `trackId`, `graph` (a clip graph, such as a shipped preset's),
`targets` (selections), `times`, `clipStart`, `clipEnd` and `seed`.

Times, clipStart and clipEnd are track seconds. The response contains the
resolved cells, musical beat positions, `UniverseState` frames and which
output capabilities the graph writes. Each target is a mapping group;
overlapping targets are rejected. Missing fixture definitions are reported
rather than replaced by invented single-head geometry. A preview does not
change the active scene or drive hardware.

Save one returned frame as JSON to render it in the real venue:

```
cargo run --manifest-path backend/Cargo.toml --bin luma-cli -- render \
  --db /path/to/disposable/luma.db --venue-id UUID \
  --state /path/to/frame.json --output /path/to/preview.png
```
