# Composable pattern contracts

The first executable slice of `docs/specs/composable-patterns.md`. This crate
owns typed graph inputs, nested graph execution, coordinate mapping, masks,
Lighting composition, and the new score document model. It has no database or
device access. The existing evaluator shares its circle solver with this crate.

Implemented examples:

- `chase` combines `chase_mask` and `appearance`.
- `pill`, `multiply_mask`, `scale_mask`, and `dissolve_mask` are ordinary graphs
  built from arithmetic, geometry, comparison, curve sampling, and randomness.
- `dissolve_mask` takes coverage (1 = all on, 0 = all off). It can hold a random
  order or refresh it every chosen beat interval. Randomness uses stable head
  identities, the clip seed, and an integer epoch, so seeking is repeatable.
- `dissolve_flash` uses the shared Envelope type for its fade curve, and keeps
  its rest dark even when the curve ends above zero.
- The version-2 `Score` directly references graph definitions from clips. Labels
  are optional, and `make_independent` copies reachable score-local subgraphs.
  Timing, selection, z-order, blend mode, seed and overrides belong to the clip.
- `Lighting` carries the existing color, dimmer, movement, strobe, and speed
  capabilities. An unwritten capability is distinct from an explicit zero.
  Color already contains brightness; effects have no second brightness input.

Mapping is an authored choice; Coordinates is a resolved runtime field. Saved
scores reject runtime cell snapshots. The host supplies selected cells and their
world/U/V/Z coordinates. Group expression resolution remains the host's job.
Circle solving reuses the existing geometry implementation. Major axis is a true
principal direction, independently normalized per requested group.

Run checks from the repository root:

```
cargo +1.97.1 test --manifest-path src-tauri/Cargo.toml -p luma-patterns
cargo +1.97.1 clippy --manifest-path src-tauri/Cargo.toml -p luma-patterns --all-targets -- -D warnings
```

`pattern-eval` reads one JSON request on stdin and writes JSON on stdout. It
supports `catalog`, `evaluate`, and `preview_score`. For example:

```json
{
  "operation": "evaluate",
  "definition": "chase",
  "cells": [
    {"id":"head-a","group":"bar","world":[0,0,0],"uvz":[0,0,0]},
    {"id":"head-b","group":"bar","world":[0,0,1],"uvz":[0,0,1]}
  ],
  "beats": [0, 1, 2, 3, 4],
  "seed": 42,
  "inputs": {
    "width": {"type":"proportion","value":0.5}
  }
}
```

```
cargo +1.97.1 run --manifest-path src-tauri/Cargo.toml -p luma-patterns --bin pattern-eval < request.json
```

## Remaining integration

Typed built-ins already run through the app's normal score evaluator and can be
inserted from its native picker. The native graph canvas hides synthetic input
wiring and supports read-only navigation into built-in graph definitions. The
version-2 score document is not yet the production persistence model: authored
revision storage, editing score-local definitions, Python authoring, and the
manual legacy migration still need to move together before adopting the reset.
The built-in library is fixed; account-level library promotion is being removed. The reference
interpreter remains an oracle for tests. `PreparedGraph` validates and flattens
the graph once, resolves fixed geometry once, and evaluates only dynamic kernels
per frame. `Score::prepare_clip` also freezes clip overrides and enforces the
clip span. Native previews use this prepared path and `BeatTimeline`, which
interpolates the detected beat grid rather than assuming constant BPM. `Rate::Fixed` inputs
reject frame-varying wires instead of silently sampling them once. Continuous
speed automation and event-latched inputs are not implemented yet.

Preserve legacy Major Span/Count behavior in migration: those old operators pick
a world axis, whereas the new Major Axis fits a principal direction. Do not
reinterpret one as the other.

## Native venue previews

The shared Tauri/headless dispatcher exposes `get_pattern_node_library` and
`preview_composable_pattern`. The latter accepts `request`:

```json
{
  "venueId": "venue UUID",
  "trackId": "track UUID",
  "definition": "dissolve_flash",
  "targets": [{"expression": "pixel_bars", "subset": {"fraction": 0.5}}],
  "times": [0, 0.25, 0.5, 0.75, 1],
  "clipStart": 0,
  "seed": 42,
  "inputs": {"travel": {"type": "beats", "value": 2}}
}
```

Times and clipStart are track seconds; exposed durations are beats. The response
contains resolved cells, musical beat positions, and `UniverseState` frames.
Each target is a mapping group; overlapping targets are rejected. Subsets count
heads after geometry expansion. Missing fixture definitions are reported rather
than replaced by invented single-head geometry. Preview retains venue and track
read authorization and does not change the active scene or drive hardware.
An optional `library` supplies custom graph definitions using the same contracts.

Save one returned frame as JSON to render it in the real venue:

```
cargo +1.97.1 run --manifest-path src-tauri/Cargo.toml --bin render_venue -- \
  --db /path/to/disposable/luma.db --venue-id UUID \
  --state /path/to/frame.json --output /path/to/preview.png
```

For repeatable execution measurements, save the response's `cells` array:

```
cargo +1.97.1 run --manifest-path src-tauri/Cargo.toml -p luma-patterns --release \
  --example frame-budget < cells.json
```

The measurement separates preparation from per-frame evaluation and includes
per-cell output construction. It does not measure the compositor, stage renderer,
or hardware output; those need their own budget during playback integration.
