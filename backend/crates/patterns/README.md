# Composable pattern contracts

This crate owns canonical score graphs, fixture × time × channel tensors,
structured controls and deterministic event sampling. It has no database,
playback-device state or separate scalar execution engine.

- `chase`, `pulse` and `dissolve_flash` emit numerical brightness signals.
  Events determine journey starts; travel sets each journey's duration. Longer
  journeys overlap with Max, and completed journeys contribute zero. A snare
  trigger and a periodic trigger connect to the same Chase input.
- `src/recipes.json` is the editable source of built-in graph recipes. Primitive
  signatures live in Rust and are the only kernel registry. The catalog does
  not depend on the migration catalog or reconstruct old graphs at startup.
- Numbers, colors and masks use the same numerical operations, with channel,
  unit and fixture-domain metadata. Scalar axes broadcast. Structured values
  such as envelopes, mapping specifications and event sources remain controls.
- Track seconds and musical beats have distinct units. Immutable beat grids,
  event spacing and event filtering are prepared once with the analyzed track.
  Preparation follows connected outputs through nested definitions: unused drum
  sources, audio analyses and clip-range reductions are removed before binding
  track data. A source offering four drums needs only the analyses actually used.
  Rebinding analysis replaces that preparation; playback and seeking share it.
  Recent-event queries return time/channel tensors. There is one editable
  Envelope value and one shared numerical sampler, broadcasting across heads,
  samples and channels. Coincident anchors describe instantaneous steps, with
  the rightmost anchor winning at the boundary. Authors draw the curve directly;
  there is no separate ADSR operation or generated stage-control graph.
- Events can target a head domain using recorded weights or deterministic random
  subsets. `random_subset` ranks by seed, event identity and head identity; counts
  round to the nearest head, including zero. Only queried event columns are
  materialized, so periodic selection has no finite table or seek boundary.
  Filtering an already targeted stream selects from its eligible heads.
  Optional Shuffled cycle visits successive groups in a seeded head order. It
  minimizes consecutive overlap for a stable eligible selection without previous
  draw state. Independent rerolls remain the default. Recent events exposes
  per-head weights for each requested event; with count one, those weights hold
  the current selection until the next event. Missing recorded events have zero
  weight, while global events broadcast their presence.
- `shimmer` composes Beat trigger → Random subset → Pulse. Reroll interval and
  envelope duration are independent; old selections finish their envelopes while
  new selections begin, combining with Max. `circle` is a four-node vector recipe
  which can feed ordinary scale/offset math for motion or sampled geometry.
- Wire rate follows its actual dependencies. Computed constants can feed fixed
  controls; time-varying wires cannot. Editable graph outputs remain able to
  accept animation after starting with a constant value.
- `core/value_noise_1d` and `core/value_noise_3d` are stateless lattice functions
  of coordinates and octaves. Their fixed seed controls preserve all 64 bits as
  decimal strings in JSON; `core/seed_stream` derives independent variations.
  Input nodes infer exact integer editors for seeds, including clip overrides.
- `audio_spectrum` emits FFT magnitudes as channels and their spacing in Hz.
  Channel indexing, arithmetic and channel sums express frequency selections;
  several consumers can share one spectrum wire. The host supplies the exact
  requested mix/stem. Missing stems are errors. New spectra are silent outside
  their audio; migrated graphs explicitly retain their old boundary hold.
- `audio_lowpass` and `audio_highpass` compose immutable audio-source requests.
  Their source and cutoff are fixed controls. The host prepares each requested
  filter chain once using the shared Butterworth filters; spectra and bands read
  that exact prepared source. Filtering starts at the source's absolute origin,
  independently of clip boundaries or playback order. Bare source values retain
  their original string encoding. A filtered source never falls back to raw audio.
- `mix_palette` projects channel weights through evenly sampled palette colors
  using a matrix product; it retains fixture/time axes and numerical headroom.
  Optional perceptual mixing operates in OKLab with a vibrance control; when
  weights match the stop count, it mixes the authored stops directly. Opacity
  is a separate output. Gradient stops preserve opacity through serialization
  and native editing; absent opacity means one.
  Gradients retain 0–64 stops: empty samples are opaque black and one stop is
  a constant color. `palette_fallback` explicitly chooses another palette only
  when the first is empty; a nonempty black palette remains black. Fixed palette
  choices fold during preparation. Native editing preserves empty values and
  supports removing the last color, adding a color, and undo.
  `core/power` broadcasts dimensionless signals. Field reductions retain their
  channel/time axes; `core/field_first` accepts an explicit fixture ordering.
- `core/channel_argmax` finds the first channel with the greatest value, reducing
  only the channel axis. `rotate_hue` accepts RGB and a rotation in turns, with
  fixture/time broadcasting. It preserves RGB extrema, including numerical
  headroom. Together with division by 12 they express pitch-driven hue rotation
  without a separate musical-color kernel.
- Output is the only terminal. It accepts independent color, dimmer, pan/tilt,
  strobe and speed signals. An unwritten capability remains distinct from zero.
  A numerical effect placed on the score gets a visible effect → Output graph;
  callers can add a color input on Output without changing the Chase node.
- Color-only Output extracts brightness. Color plus an explicit dimmer keeps
  the two independent. Perceptual gradient interpolation remains OKLab.
- `Score` defaults to version 3. Named Input nodes preserve stable keys while
  exposing destination-inferred controls; renaming a label does not lose clip
  overrides. Timing, selection, layering and the overall random seed belong to
  the clip; individual noise operations can also expose an explicit seed.
- Spatial recipes use arithmetic, ranking, field reductions, envelopes and
  deterministic random thresholds. Custom UVZ vectors supplement mapping
  presets; a mirror has an independent plane normal and offset.
- `wander_points` emits bounded, deterministic XYZ triples in consecutive signal
  channels. Time, count, seed, bounds, drift, amplitude and axis periods are
  explicit inputs. `proximity_weights` accepts these or custom point triples,
  and maps an arbitrary XYZ position signal to one weight per site. Zero blend
  distance selects the nearest site, sharing ties; positive distance gives
  normalized exponential weights. Neither operation reads fixture layout or
  colors. A caller can supply world positions or sample another spatial domain.

Mapping is an authored choice; Coordinates is a resolved runtime field. Saved
scores reject runtime cell snapshots. The host supplies selected cells and their
world/U/V/Z coordinates. Group expression resolution remains the host's job.
Circle solving reuses the existing geometry implementation. Major axis is a true
principal direction, independently normalized per requested group.

Run checks from the repository root:

```
cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma-patterns
cargo +1.97.1 clippy --manifest-path gpui/Cargo.toml -p luma-patterns --all-targets -- -D warnings
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
  "clip_duration": 8,
  "seed": 42,
  "inputs": {
    "width": {"type":"proportion","value":0.5}
  }
}
```

```
cargo +1.97.1 run --manifest-path gpui/Cargo.toml -p luma-patterns --bin pattern-eval < request.json
```

## Host integration and migration

`PreparedGraph` flattens once and executes each operation once per requested time
batch. Event kernels form a temporary event axis and reduce it; they do not
retain journeys or replay a graph for each event. `Score::prepare_clip` freezes
clip inputs and enforces the clip span. Native visualizer previews share this
path, including real track audio, beat-grid interpolation and argument overrides.

Opening an owned v2 score or a row-based score of typed patterns records its v3
conversion through authored history. Referenced standalone patterns become shared
score-owned graphs, preserving names, controls, overrides and layout. Clip seconds
map through the real tempo grid. A historical selection seed remains separate
from effect randomness. Read-only previews convert a copy; historical source
bytes and revisions remain unchanged. Restoring an old source and reopening it
creates a new migration operation against that restored history head.

Frozen vocabularies live under `migrations/`. Historical validation reads their
schemas without executing retired kernels. Standalone typed playback also lowers
through that converter before execution. Old scalar/broadcast/bundle writer and
Travel Clock runtime branches are removed. Older untyped pattern/document
migration and removal of the remaining host evaluator are still integration work.

Preserve legacy Major Span/Count behavior in migration: those old operators pick
a world axis, whereas the new Major Axis fits a principal direction. Do not
reinterpret one as the other.

## Native venue previews

The shared native/headless dispatcher exposes `get_pattern_node_library` and
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
cargo +1.97.1 run --manifest-path gpui/Cargo.toml --bin render_venue -- \
  --db /path/to/disposable/luma.db --venue-id UUID \
  --state /path/to/frame.json --output /path/to/preview.png
```

For repeatable execution measurements, save the response's `cells` array:

```
cargo +1.97.1 run --manifest-path gpui/Cargo.toml -p luma-patterns --release \
  --example frame-budget < cells.json
```

The measurement separates preparation from per-frame evaluation and includes
per-cell output construction. It does not measure the compositor, stage renderer,
or hardware output; those need their own budget during playback integration.

`clip_range` prepares a sampled minimum and maximum over the placed clip, across
all heads, time samples and channels, retaining the signal's unit. It defaults to
1,024 evenly spaced musical-time samples (including the endpoints); this is an
estimate, with adjustable resolution for faster or longer signals. Preparation
uses bounded batches and only the upstream dependency graph. Nested ranges prepare
in dependency order, and playback reuses the result. Rebinding track analysis
recomputes it. Normalize and Invert over clip are five-node arithmetic recipes
using the same range, rather than separate evaluator kernels.
