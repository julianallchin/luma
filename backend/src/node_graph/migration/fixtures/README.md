# Original graph references

`shared-palettes-v1.json` records ten cases that share one exposed palette among
harmonic palette nodes, an ordinary sampler and Soft Voronoi. It covers empty
Palette/Gradient arguments, single transparent colors, two colors, connected
versus local harmonic fallbacks, chord changes and repeated seeks.
`capture_empty_palettes.rs` captures only the original category evaluator and
asserts that no canonical graph operation participates. The Color and Spatial
kernel files matched commit `7cfc6e8faf6ee583e7b50830d1878cbde12a8270` byte-for-byte
at capture (Git blob hashes `928f8f05b3c2ca426d40c1cb4be5501a93dc36ab` and
`ed6288c2f0e1614a9a9a1143a4abfed5d90b427e`).

Conversion preserves empty data instead of substituting a color during decoding.
Ordinary samplers produce opaque black; harmonic consumers explicitly select
their rainbow fallback when the connected palette is empty, ignoring their local
fallback in that case. Unconnected harmonic consumers retain their local colors.
Single stops stay single and retain position/opacity. All ten captures compare
at the existing tolerance. Additional regressions change the default to white,
apply empty clip overrides, rename the shared Input and round-trip saved data.
Temporarily include the capture module from numerical.rs and run:

```sh
LUMA_CAPTURE_SHARED_PALETTES=/tmp/shared-palettes-v1.json cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma --lib capture_original_shared_palettes
```

`spectral-v1.json` contains 153 original hue-shift cases: every pitch class,
black/white/gray/near-gray/saturated/mixed colors, equal weights, and negative
weights. `capture_spectral.rs` is a standalone capture of the HSL formula copied
unchanged from commit `71320b4a05f97d2edbc2fa10f5ebd8a5dec2c58d`'s
node implementation. It does not
import the replacement evaluator. The category compiler omitted Spectral Shift,
so this capture uses the earlier implementation rather than inventing a category
rendering reference.

Conversion expresses hue shifting through channel argmax, division and hue
rotation. It explicitly retains first-selected-head broadcasting, ignores the
historically ineffective Strength parameter, and discards alpha as the old
operation did. The original `-1` floor for pitch weights is explicit arithmetic.
All reference values compare at 3e-5; generic hue rotation separately proves
fixture/time broadcasting, inverse rotation and preservation of RGB headroom.

`voronoi-v1.json` contains 380 original Soft Voronoi cases across empty, single,
flat and volumetric layouts. It covers empty/one-color/three-color/uneven/transparent
palettes, clamped and fractional point counts, hard/soft regions, vibrance, speed,
seed offsets, negative time and repeated seeks. Both spatial compiler and spatial
kernel files matched commit `7cfc6e8faf6ee583e7b50830d1878cbde12a8270` at capture.
`capture_voronoi.rs` records the original evaluator's raw RGBA values and frames;
it never calls the replacement geometry or palette kernels.

Converted graphs compose moving point signals, proximity weights and perceptual
palette mixing. Point time is explicit seconds, independent of detected tempo.
The original low-discrepancy starts, seeded motion, reflected bounds, palette
resampling and chroma correction are preserved at the existing 3e-5 tolerance.
Opacity stays a separate numerical output and is rejoined into RGBA only for
historical consumers. The old `bounds` comment claimed a minimum axis extent;
the implementation did not apply one. Migration follows the actual bounds,
including zero-width axes. No reference values or tolerances were changed.

`selection-v1.json` contains 672 original Random Select Mask cases: zero, one
and five heads; beat, kick, snare and unwired triggers; two clip starts; negative,
zero, fractional and oversized counts; both avoid-repeat settings; and two node
identities. `capture_selection.rs` records the original category evaluator's raw
signals and output frames. The original captures remain unchanged.

Migration preserves selected counts, event timing, held selections and the old
clip-start boundary. Exact head sequences intentionally change: independent
draws use stable head identities instead of fixture indices, and Avoid Repeat
becomes Shuffled cycle. Successive groups in one seeded permutation minimize
adjacent overlap without replaying earlier draws. With N heads and K selected,
adjacent overlap is max(0, 2K−N). The cycle repeats after N/gcd(N,K) events for
0 < K < N. This guarantees consistent brightness/count and fair coverage; it
does not reproduce the old conditioned random sequence. New Random subset
defaults to independent rerolls, including the per-head Shimmer recipe.

Tests compare all captured counts and black intervals, then check hold timing,
minimum overlap, batch versus individual seeks, reordered fixtures, shared named
Count inputs, renaming and serialized clip overrides. Core tests also seek a
trillion events forward and backward without materializing previous events.

`events-v1.json` adds 960 original Beat Pulses, Beat Envelope and ADSR cases.
It covers regular/variable tempo, empty/nonempty fixture selections, all four
drum channels, subdivisions, offsets, downbeats, anticipation, short/dense/empty
event lists, zero/unequal ADSR ratios, curve bias, amplitude and repeated seeks.
The reference includes exact drum timestamps and samples at pulse boundaries.
The signal compiler and kernels were unchanged from the original commit during
capture (`git diff 7cfc6e8faf6ee583e7b50830d1878cbde12a8270` on those files was empty).
`capture_events.rs` records expected frames and raw taps only through that old
evaluator; the musical clock supplies fixture metadata, not expected samples.
Temporarily include the generator as a test module and run:

```sh
LUMA_CAPTURE_EVENT_MIGRATION=/tmp/events-v1.json cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma --lib capture_original_event_graphs
```

Beat Pulses historically represented both a sampled numeric pulse and a traced
event source. Conversion separates those meanings. Recorded events retain f32
seconds precision, 25 ms greedy drum coalescing, and the original grid alignment.
ADSR fit-to-gap uses the minimum positive gap over the full event list; its fixed
length fallback used 120 BPM. It considers the latest two events, unlike Chase's
full active-event axis. These rules are represented by indexed event queries and
editable signal graphs. A separate regression shares an exposed control through
arithmetic and checks its clip override against the frozen output.

The captured downbeat checkbox values are numeric, as stored by the old UI.
The old event-tracing path ignored a JSON Boolean `true` even though the direct
pulse path recognized it. Conversion consistently honors that checkbox; this is
an explicit correction for hand-authored Boolean parameters, not a parity claim
for the old inconsistent interpretation.

`audio-v1.json` adds 160 frequency/stem cases over 8 kHz, 44.1 kHz and 44,101 Hz audio,
empty/nonempty fixture selections, all five audio sources, disjoint/overlapping
ranges, reversed/out-of-band bounds and invalid/empty range defaults. It stores
the exact input PCM samples used by the original evaluator, so replay does not
depend on regenerating tones with a platform's trigonometry implementation.
Sample times include the leading FFT padding, both audio boundaries, a later
seek and a repeated time. Raw comparisons use a tighter audio tolerance of
max(2e-8, |expected| × 5e-5); the original reductions accumulated in f32.

`capture_audio.rs` follows the isolated-checkout procedure below, declaring
`mod capture_audio`, then running:

```sh
LUMA_CAPTURE_AUDIO_MIGRATION=/tmp/audio-v1.json cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma --lib capture_original_audio_graphs
```

The audio compiler and kernels matched the original commit during the first
140 captures. The causal FFT implementation then moved to `audio/spectrum.rs`, shared
by canonical spectrum sampling and the remaining historical callers. Conversion
expresses band weighting and normalization through ordinary tensor arithmetic,
channel indices and channel sums, including counting overlapped bins twice.
Historical hold-at-audio-boundary behavior is explicit in migrated graphs. New
spectrum nodes default to silence outside the audio. Missing requested stems
now fail explicitly; substituting the full mix is not a preserved behavior.
The additional 20 cases exercise rounding at a bin boundary at 44,101 Hz. Their
capture also reran the first 140 and confirmed identical frozen output. Old band
division rounded to f32 before floor/ceil; conversion must preserve that rounding
or it can include a neighboring bin even though the spectrum itself is correct.

`noise-v1.json` adds 306 original Noise/Wander cases over empty, single-fixture
and three-fixture selections. Three node identities exercise the original seed
hash, with multiple octave, scale, amplitude and movement settings. Noise cases
cover every combination of X/Y/time connections, including a fixture-valued time
input with no spatial input. Original Noise followed the X/Y domain only; a
missing X with connected Y used the original selection index. Conversion makes
those two behaviors explicit with domain alignment and fixture-index operations.
Seed values retain all 64 bits as fixed controls serialized as decimal strings.
The capture includes raw signal/UV taps and color/movement outputs at six times,
including a repeated seek. It never uses the new noise kernels or converter to
generate expected values.

`capture_noise.rs` follows the isolated-checkout procedure below, declaring
`mod capture_noise`, then running:

```sh
LUMA_CAPTURE_NOISE_MIGRATION=/tmp/noise-v1.json cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma --lib capture_original_noise_graphs
```

The original signal compiler and kernels match the original commit below.
Preserving their f32 interpolation and octave accumulation is necessary to keep
authored random textures and movement paths unchanged.

`harmony-v1.json` adds 110 cases: Falloff over scalar, RGB, spatial and twelve-
channel signals, and pitch palettes over every pitch class, overlapping/no-chord
sections, gaps, mixed/negative/very small weights and short vectors. Five cases
exercise weights that vary by fixture: the original palette sampled the first
selected fixture and broadcast its color. Conversion expresses that explicitly
using fixture order and a first-value reduction. It does not depend on canonical
row storage. The original unconnected palette panicked when reading input zero;
that invalid-graph case cannot supply an output reference and is covered by a
separate black-output regression for the replacement.

`capture_harmony.rs` follows the same isolated-checkout procedure below,
declaring `mod capture_harmony`, then running:

```sh
LUMA_CAPTURE_HARMONY_MIGRATION=/tmp/harmony-v1.json cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma --lib capture_original_harmony_graphs
```

The original signal/audio compilers and signal/color/audio kernels were unchanged
during capture. Palette parsing uses the shared migration codec; its valid color
behavior is unchanged. This fixture records chord sections alongside the original
frames and raw taps; it does not generate expected values through the converter.

`spatial-v1.json` contains six fixture layouts and 546 cases covering every
original spatial attribute/alias, each mirror axis, raw folded positions and
mirror-side values. It includes empty, single-head, vertical, flat, asymmetric
and tilted-circle layouts. Fixture IDs intentionally run in reverse lexical
order. `capture_spatial.rs` follows the same isolated-checkout procedure below,
declaring `mod capture_spatial`, then running:

```sh
LUMA_CAPTURE_SPATIAL_MIGRATION=/tmp/spatial-v1.json cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma --lib capture_original_spatial_graphs
```

The spatial compiler and kernels match the original commit below. The existing
circle fitter has acquired fields for projecting additional points; its captured
fit and angular-position computation are unchanged. The new geometry operations
preserve f32 fitting precision and expose sample order explicitly: both can
change the chosen basis on symmetric or near-isotropic point clouds. Raw tensor
comparisons resolve rows by fixture identity, rather than assuming matching row
storage. Spatial outputs also exercise direct Apply Dimmer above one, which must
retain headroom until master compositing.

`numerical-v1.json` contains 62 graphs and their original evaluator results at
six sample times, including a repeated time. Raw visualizer taps are included so
output clipping cannot hide incorrect arithmetic. The evaluator is from commit
`7cfc6e8faf6ee583e7b50830d1878cbde12a8270`; the original numerical kernels were
unchanged during capture. These references never run the converter to generate
expected values.

`movement-v1.json` adds 37 cases from the same unchanged evaluator: Circle,
Figure 8 and Sweep, both direct and composed with scalar/RGB arithmetic. Each
captures raw UV taps and both movement/color sinks. `capture_movement.rs` follows
the same isolated-checkout procedure, declaring `mod capture_movement`, with:

```sh
LUMA_CAPTURE_MOVEMENT_MIGRATION=/tmp/movement-v1.json cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma --lib capture_original_movement_graphs
```

The original compiler passed UV values directly to pan/tilt angles; its comments
described a movement pyramid that was not executed. Conversion preserves the
executed values. Old arithmetic repeated the last component of a shorter vector;
conversion now makes that padding explicit with channel extraction and joins.
New numerical connections use normal broadcasting and reject incompatible widths.

`capture_numerical.rs` is the independent generator. In an isolated copy of that
revision, copy it to `backend/src/capture_numerical.rs`, declare
`#[cfg(test)] mod capture_numerical;` in `backend/src/lib.rs`, and run:

```sh
LUMA_CAPTURE_NUMERICAL_MIGRATION=/tmp/numerical-v1.json cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma --lib capture_original_numerical_graphs
```

The active regression test uses the frozen data only. It checks raw arithmetic
and channel counts within 3e-5 (the original used f32, the new core uses f64),
and normalized fixture output. The raw capture retains the original out-of-range
RGB and dimmer values. The new Output retains positive dimmer headroom until the
compositor applies master intensity. All cases compare dimmer at master levels
0, 0.25, 0.7 and 1, both alone and over a lower layer, through the playback adapter.
For valid original RGB, layer color/opacity is checked too. Negative historical
chromaticity remains an explicit difference: the new capability boundary clamps
it, and those colors are excluded from the layer-color parity assertion. Raw
arithmetic taps are still checked without normalization.

The constant-tempo reference does not establish equality across tempo changes.
New oscillators use the actual musical timeline; the original used absolute
seconds multiplied by the track's single BPM value. Original Time Delay executed
as identity, so its migration reconnects the input directly.

### Envelope conversion after the curve-model correction

Saved ADSR stage settings become an ordinary editable Envelope at import.
Linear shapes use five anchors; biased stages are fitted to cubic Bézier segments
once during migration. The old stage controls were parameters, not connectable
ports. The unnecessary connected-stage adapter and generated-points operation
have been deleted. Trigger/subdivision connections remain supported.

All 960 frozen event cases use the original 3e-5 tolerance. All-zero stage
durations are intentionally silent: the old evaluator held sustain for an extra
100 microseconds, producing 144 nonzero raw samples across these captures. The
regression test identifies only those all-zero controls, verifies the captured
flash values, then compares against silence. Capture files remain unchanged.

Separate tests cover steep positive/negative biases, negative and overdriven
levels, serialization, anchor editing, exposed trigger controls, and compilation
of historical graphs through the shared Envelope sampler.

### Whole-clip reductions

`reductions-v1.json` contains 160 captured Normalize/Invert cases: scalar, ramp,
sine, circle vectors and constant noise; empty/nonempty fixture selections;
constant/variable beat intervals; two clip spans; and nested reductions. Capture
used the original numerical kernels and 44 Hz sampling arithmetic. Comparison
against `7cfc6e8faf6ee583e7b50830d1878cbde12a8270` verified that the only change to
the range preparation function was error propagation. The replacement was not
used to produce expected values. Register `capture_reductions.rs` temporarily as
a test module and set `LUMA_CAPTURE_REDUCTIONS` to an output copy when recapturing
from that original evaluator.

The canonical range uses 1,024 evenly spaced musical-time samples, including both
clip endpoints. Single-reduction comparisons measure the changed range estimate,
not a playback-state difference: maximum raw deviations are 0.001601 for sine,
0.000337 for circle and below 1e-7 for scalar/ramp/constant noise. The regression
bounds the new comparisons at 0.002; no earlier fixture tolerance was widened.
That is under one 8-bit intensity step. This does not claim exact extrema for
arbitrary signals; the node exposes sampling resolution.

Nested reductions deliberately change. The old preparation sampled every node
before any range was finalized, so Normalize→Normalize and Normalize→Invert
collapsed to zero. The captures retain that evidence. Tests require the new
composition to be identity after normalization, or its reflection for Invert,
and check repeated/out-of-order seeks against batches.

The variable-tempo captures also exposed an earlier migration gap: historical
oscillators used seconds and the track's average BPM, and ramps used clip-local
seconds. Imported graphs now express that timing explicitly through Track time
metadata and ordinary math. New authored effects keep the musical-time model.

### Mixed historical vocabularies

The old compiler rejected graphs containing both typed pattern calls and the
original numerical nodes, so there is no original mixed-playback capture.
`numerical/tests/mixed.rs` instead checks an analytic sine → typed multiply →
old add → typed motion composition, plus strobe and dimmer. It verifies shared
and renamed inputs, overrides, seeks, one terminal, and absent color/speed writes.
Old bundle composition is lowered by the same v2 migration used for typed scores;
only the capability channels actually present are passed between helpers.

Mixed graphs with competing writers for a capability require explicit composition.
Malformed wires, missing nodes and duplicate sources fail instead of losing data.
The authored-document test repeats the existing database/history migration proof
with a typed Chase control routed through an original scalar node. It checks
shared clip definitions, timing, arguments, selection seeds, safe candidate
creation, concurrent retries, restore, and unchanged library source bytes.

### Restored audio filter chains

At `2e193e4b^`, `src-tauri/src/node_graph/nodes/audio.rs` applied the shared
Butterworth low/high-pass functions to PCM and passed the resulting audio to
downstream analysis. The later category compiler stopped recognizing these nodes.
Its separate windowed-filter/RMS kernels were never wired to those graph nodes;
they also disagreed with the advertised audio-output interface and are now removed.

Saved filter nodes now form immutable source → filter → spectrum requests.
The original cutoff floor of 1 Hz and sample-rate-dependent upper clamp remain.
The filter coefficients and shared DSP implementation are unchanged. Tests compare
prepared PCM to the original filter functions, verify frequency rejection, chain
order, selected stems, shared preparation, negative cutoff migration, serialization
and seek consistency. There is no claim of a working category-compiler filter
capture; that compiler returned UnknownNode.

Preparation now starts at the source's absolute beginning, instead of resetting
filter state at each cropped clip boundary. This intentionally removes clip-start
transients and makes overlapping clips agree about the same filtered audio. The
processing happens during feature preparation, never by retaining playback state.
