# Composable pattern contracts

This crate owns the clip forms, the graphs they are built from, fixture ×
time × channel tensors and structured controls.
It has no database, playback-device state or separate scalar execution engine.

- `src/recipes.json` holds the few shared graphs the forms are built from:
  Normalize, Uniform mask, Mapped position and Strobe output. Primitive
  signatures live in Rust and are the only kernel registry.
- Numbers, colors and masks use the same numerical operations, with channel,
  unit and fixture-domain metadata. Scalar axes broadcast. Structured values
  such as envelopes and mapping specifications remain controls.
- Track seconds and musical beats have distinct units. There is one curve
  format (`Curve`) and one shared sampler, broadcasting across heads, samples
  and channels. A curve is `{"points": [[x, value], [x, value, ease], ...]}`
  with x strictly increasing from 0 to 1. The ease moves the value from its
  point to the next: `"linear"` (the default, never written), `"ease-in"`,
  `"ease-out"`, `"ease-in-out"`, `"hold"` (jump at the next point) or a CSS
  `cubic-bezier` `[x1, y1, x2, y2]` local to the segment. The last point has
  no ease. An `Envelope` is a curve of values 0..1.
- Wire rate follows its actual dependencies. Computed constants can feed fixed
  controls; time-varying wires cannot. Editable graph outputs remain able to
  accept animation after starting with a constant value.
- `band_energy` reads the energy of a frequency band of the track's full mix.
  The host prepares the mix once; a missing analysis is an error.
- Every light color is linear Rec. 2020, 0–1 per channel, with no tag:
  stored colors, gradient stops, color keyframes, presets, the compositor
  and `FixtureOutput`. Brightness is the peak channel. `src/color_space.rs`
  holds the conversions: OKLab for blending, sRGB hex for people, and
  `Gamut` for emitters. A color that a fixture's emitters or the display
  cannot make is mapped to the nearest one they can, in OKLab, by reducing
  chroma at constant lightness and hue. A fixture's red, green and blue are
  assumed to have sRGB primaries.
- Gradients interpolate perceptually in OKLab and keep stop opacity through
  serialization and native editing; absent opacity means one.
- Output is the only terminal. It accepts independent color, dimmer, pan/tilt,
  strobe and speed signals. An unwritten capability remains distinct from zero.
- Color-only Output extracts brightness. Color plus an explicit dimmer keeps
  the two independent. Perceptual gradient interpolation remains OKLab.
- Timing, selection, layering and the random seed belong to the clip.
- Custom UVZ vectors supplement mapping presets; a mirror has an independent
  plane normal and offset. Radial and angle need a plane and read around the
  centroid of the span.

Mapping is an authored choice; Coordinates is a resolved runtime field. The
host supplies selected cells and their world/U/V/Z coordinates. Group
expression resolution remains the host's job. Major axis is a true principal
direction, independently normalized per requested group.

Run checks from the repository root:

```
cargo +1.97.1 test --manifest-path backend/Cargo.toml -p luma-patterns
cargo +1.97.1 clippy --manifest-path backend/Cargo.toml -p luma-patterns --all-targets -- -D warnings
```

`pattern-eval` reads one JSON request on stdin and writes JSON on stdout. It
supports `catalog`, `evaluate`, and `preview_score`. For example:

```json
{
  "operation": "evaluate",
  "definition": "color@1",
  "cells": [
    {"id":"head-a","group":"bar","world":[0,0,0],"uvz":[0,0,0]},
    {"id":"head-b","group":"bar","world":[0,0,1],"uvz":[0,0,1]}
  ],
  "beats": [0, 1, 2, 3, 4],
  "clip_duration": 8,
  "seed": 42,
  "inputs": {
    "every": {"type":"beats","value":2}
  }
}
```

```
cargo +1.97.1 run --manifest-path backend/Cargo.toml -p luma-patterns --bin pattern-eval < request.json
```

## Clip forms

The shipped forms are `color@1`, `aim@1` and `strobe.constant@1`. Every
clip holds all its form's inputs; missing or unknown inputs are errors.
Color has color, brightness and clip fade. Aim has a position and numeric
spatial/horizontal/vertical offsets. Presets are saved inputs.

Inputs use Time, Space, Random, Noise and Audio sources. Source numeric
inputs recursively accept sources. Time and Random own events, inherit an
enclosing clock, or follow another input with `events.same_as`. Space offset
from Time produces overlapping strokes. Brightness keeps the strongest
event; a following color belongs to that event. Clip fade runs once over
the whole clip. Shared grain groups heads, fixtures or clumps.

Forms lower into the shared tensor graph during `PreparedGraph` preparation.
Period sources integrate into fixed clock tables; evaluation has no playback
history. Event channels travel on separate wires from RGB/vector components.
Kernels evaluate whole fixture × time × event tensors. Static work folds once;
Random ranks groups once per event, rather than once per head. There is no
separate form interpreter.

See [the source model](../../../docs/design/2026-09-28-sources-implementation.md)
for schema, migration and verification details.

## Host integration

`PreparedGraph` flattens once and executes each operation once per requested time
batch. `Score::prepare_clip` freezes
clip inputs and enforces the clip span. Native visualizer previews share this
path, including real track audio, beat-grid interpolation and argument overrides.

## Native venue previews

The shared native/headless dispatcher exposes `preview_composable_pattern`. It
accepts `request`:

```json
{
  "venueId": "venue UUID",
  "trackId": "track UUID",
  "definition": "color@1",
  "targets": [{"expression": "pixel_bars"}],
  "times": [0, 0.25, 0.5, 0.75, 1],
  "clipStart": 0,
  "seed": 42,
  "inputs": {"color": {"type":"color","value":[1,1,1]}, "brightness": {"type":"proportion","value":1}, "fade": {"type":"proportion","value":1}}
}
```

Times and clipStart are track seconds; exposed durations are beats. The response
contains resolved cells, musical beat positions, and `UniverseState` frames.
Each target is a mapping group; overlapping targets are rejected. Missing
fixture definitions are reported rather than replaced by invented single-head
geometry. Preview retains venue and track
read authorization and does not change the active scene or drive hardware.

Save one returned frame as JSON to render it in the real venue. `render_venue`
is a backend binary:

```
cargo +1.97.1 run --manifest-path backend/Cargo.toml --bin render_venue -- \
  --db /path/to/disposable/luma.db --venue-id UUID \
  --state /path/to/frame.json --output /path/to/preview.png
```

`clip_range` prepares a sampled minimum and maximum over the placed clip, across
all heads, time samples and channels, retaining the signal's unit. It defaults to
1,024 evenly spaced musical-time samples (including the endpoints); this is an
estimate, with adjustable resolution for faster or longer signals. Preparation
uses bounded batches and only the upstream dependency graph. Nested ranges prepare
in dependency order, and playback reuses the result. Rebinding track analysis
recomputes it. Normalize is a five-node arithmetic graph using the same range,
rather than a separate evaluator kernel.
