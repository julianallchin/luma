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
- Track seconds and musical beats have distinct units. There is one editable
  Envelope value and one shared numerical sampler, broadcasting across heads,
  samples and channels. Coincident anchors describe instantaneous steps, with
  the rightmost anchor winning at the boundary.
- Wire rate follows its actual dependencies. Computed constants can feed fixed
  controls; time-varying wires cannot. Editable graph outputs remain able to
  accept animation after starting with a constant value.
- `band_energy` reads the energy of a frequency band of the track's full mix.
  The host prepares the mix once; a missing analysis is an error.
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
  "definition": "color.chase@1",
  "cells": [
    {"id":"head-a","group":"bar","world":[0,0,0],"uvz":[0,0,0]},
    {"id":"head-b","group":"bar","world":[0,0,1],"uvz":[0,0,1]}
  ],
  "beats": [0, 1, 2, 3, 4],
  "clip_duration": 8,
  "seed": 42,
  "inputs": {
    "travel": {"type":"beats","value":2}
  }
}
```

```
cargo +1.97.1 run --manifest-path backend/Cargo.toml -p luma-patterns --bin pattern-eval < request.json
```

## Clip forms

A form is a shipped graph with a fixed interface: `color.constant@1`,
`color.time@1`, `color.space@1`, `color.chase@1`, `color.sparkle@1`,
`color.noise@1`, `strobe.constant@1` and `aim@1` (see
`docs/specs/clip-forms.md` and `docs/specs/aim.md`).
A form clip sets `graph` to the form id and holds a value for every input.
A missing or unknown input is an error. A score holds form clips only.

- An input takes a plain value or, where its `promotable` list allows, a
  source: `time` and `hit` keyframe curves, `noise`, or `audio` (a band of
  the full mix, scaled over the clip). Sources are tagged values, for example
  `{"type":"time","value":{"points":[[0,2],[1,0.5]],"segments":["linear"]}}`.
- `PreparedGraph::new` lowers each source into nodes of a copy of the form.
  A `time` curve on a speed input (`every`, `travel`, `duration`, `speed`)
  is summed over the clip like an odometer, from a table built from the
  curve, so a sought frame equals a played frame.
- `core/event_life`, `core/odometer`, `core/curve`, `core/random_share`,
  `core/path_glides` and the `core/aim_*` steps are the primitives only the
  forms use. A period or life of 0 beats lasts the whole clip.
- `presets()` reads `src/presets.json`: named presets (a form and every
  input value) and named curves for `time` and `hit` sources.

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
  "definition": "color.chase@1",
  "targets": [{"expression": "pixel_bars"}],
  "times": [0, 0.25, 0.5, 0.75, 1],
  "clipStart": 0,
  "seed": 42,
  "inputs": {"travel": {"type": "beats", "value": 2}}
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
