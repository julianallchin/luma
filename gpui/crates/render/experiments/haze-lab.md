# Haze quality lab

Build and pin inputs before changing transport. `haze-lab` runs the production
renderer without the app, audio, DMX or database writes. It saves the actual
settings, camera, GPU timings and PNGs for every case. `render_next` drives live
captures; `render_reference` is used only for explicitly named high-sample references.

```sh
cargo +1.97.1 build --manifest-path gpui/Cargo.toml -p luma-render --bin haze-lab
# A self-contained physical shadow case: one Atomic 3000, 1.5 metres behind a truss.
gpui/target/debug/haze-lab capture gpui/crates/render/experiments/haze-shadow-suite.json /tmp/haze/live live
gpui/target/debug/haze-lab capture gpui/crates/render/experiments/haze-shadow-suite.json /tmp/haze/reference reference128
```

For a saved score, `profile_score SCORE_ID OUTPUT_JSON 960 640 12 snapshot`
exports evaluated states at 1, 3 and 7 seconds through a read-only database
connection. Build that binary with the backend's `perf` profile for CPU timing;
see `gpui/BUILD.md` before selecting a target directory. The export records the
head keys actually requested by the production frame builder. Once exported,
all experiments consume that immutable catalogue, not the changing library.

```sh
gpui/target/debug/haze-lab prepare /tmp/gasworks.json /tmp/haze/gasworks
gpui/target/debug/haze-lab capture /tmp/haze/gasworks/suite.json /tmp/haze/live live
```

Prepare records six views per score snapshot: wide, side, inside, broad source,
source from the side, and close LED bar (where those fixture types exist).
Inspect the first captures and edit `suite.json` to pin useful angles. Coordinates
in the suite are renderer Z-up, unlike the Y-up `Scene.camera` fields.
Catalogue paths in a hand-written suite resolve relative to that suite.

## Current result

Measured 2026-09-09 on an RTX 5090. The live renderer keeps fine shadow
intervals at native pixel resolution:

- Native per-pixel integration walks cached min/max shadow maps to find the lit
  intervals. Four-point Gaussian quadrature integrates only those spans, in
  equiangular coordinates. Thin shadow shafts need no random ray samples.
- A shared far-field grid (`fog_grid.rs`) carries broad lighting. Conservative
  4×4×4 block bounds prove groups of cells wholly lit or dark.
- Camera depth bounds skip prefixes that no visible pixel reads. Density and
  camera transmittance are integrated once and shared across lights.
- Fixed fixture shadows and their min/max mipmaps survive colour, dimmer and
  blackout changes. Final coloured radiance is never cached across cues.

The Get Lucky / Gasworks replay at 1634×750 measured 3.95–4.14 ms median and
5.39–5.60 ms p95 GPU time over two 1,152-frame runs. These are GPU timings. They
exclude GPUI, picking, presentation and readback, and are not live FPS.

Against the exhaustive 64×32 reference, display-space RMSE over 26 pinned views
was 0.17–1.06 on a 0–255 scale. Eight repeated frozen frames had zero temporal
variance. Turning off both conservative visibility optimisations changed at most
53 pixels per capture by one code level.

The interval approach follows Chen et al., *Real-Time Volumetric Shadows using 1D
Min-Max Mipmaps* (https://groups.csail.mit.edu/graphics/mmvs/). Luma uses a
camera-independent 2D shadow hierarchy, not their epipolar structure.

Limits:

- The reference shares the single-scattering model, the source-extinction cache
  and the fixture shadow maps. Agreement with it does not prove photographic
  realism.
- Far-field lighting has finite grid precision. Gobos stay stochastic.
- Fixture shadow cameras cap their field of view at 170°. Very wide washes are
  unshadowed outside that projection.

## Experiments

Each invocation starts a fresh process. Use a clean environment: externally set
`LUMA_*` renderer switches remain effective and are recorded in `capture.json`.
Keep each variant in its own output directory.

| Mode / switch | Change from live defaults |
| --- | --- |
| `live` | Native-resolution deterministic beams, shared far-field grid, production subframe budget |
| `no-haze` | Zero haze density, to isolate the stage surface and submission path |
| `uniform` | Cloudiness zero, to distinguish physical cloud variation |
| `no-grid` | Per-ray broad-light integration, reservoir selection retained |
| `exact-live` | Diagnostic MIS integration, every light evaluated at native resolution; 8 samples retained |
| `full-live` | Same native target as current live defaults |
| `reference16/32/64/128` | Native-resolution haze, every light, 32 integration samples, stated accumulated subframes, grid disabled |
| `LUMA_FOG_TILE_SIZE=4/8/16/32` | Diagnostic grid resolution sweep; default 16 on macOS, 8 elsewhere, fixed for process lifetime |
| `LUMA_FOG_BLOCKS=0` | Disable shared block visibility proofs; evaluate every candidate cell/light |
| `LUMA_FOG_DEPTH_CULL=0` | Retain all column prefixes, including those hidden behind geometry |
| `LUMA_SURFACE_DEPTH_CULL=0` | Disable the conservative MSAA-depth refinement of surface light masks |
| `LUMA_HAZE_COMPUTE=0/1` | Select fragment/compute execution for deterministic grid haze with complete shadow coverage; default compute on Metal, fragment elsewhere |
| `LUMA_HAZE_WORK_COUNTS=1` | Count native compute-haze work and emit `work-counts.json`; instrumented timings are ineligible for performance comparisons |
| `LUMA_FOG_GRID_COUNTS=1` | Diagnostic `fog-grid` kernel (`haze_grid_counted.wgsl`) counting every `segment_shadow_visibility` outcome (proven lit / proven shadowed / 4-tap fallback) and what a 4-cell column-block union proof would give; emits `fog-grid-counts.json` after `still-7`; timings ineligible |
| `LUMA_PROFILE_OMIT=NAME` | Diagnostic shader omission; changes the image and cannot pass the exact quality gate |
| `LUMA_PROFILE_REPEAT=PASS` | Repeat one idempotent pass with identical inputs to measure incremental work; outputs must remain exact and timings are performance-ineligible |
| `LUMA_VISIBILITY_REFERENCE=1 LUMA_GEOMETRY_SHADOW_SAMPLES=32` | Software triangle visibility instead of cached shadow maps; use with a reference mode |

The reference converges our current single-scattering model. It is not an
independent path-traced physical ground truth: source extinction still uses its
angular cache, density has finite quadrature, and accumulation uses half-float
render targets. Compare two reference sample counts before treating small image
differences as meaningful. A triangle-visibility reference additionally removes
the shadow-map approximation, at considerable cost for dense geometry.

`LUMA_PROFILE_OMIT` accepts `surface-clouds`, `surface-lighting`,
`surface-shadows`, `face-lights`, `native-shadows`, `native-integrals`,
`native-clouds`, `native-light-depth`, `native-camera-depth`, or
`grid-shadow-tests`. Unknown values fail at pipeline creation. An invocation
with this variable set is tagged both `qualityReferenceEligible: false` and
`performanceReferenceEligible: false`, and the
Mac exact pixel audit rejects it. Omissions specialize only the named consumer:
native omissions leave grid lighting and surface shading intact, and grid shadow
omission leaves block classification intact. `native-clouds` retains the physical
medium envelope; the two depth omissions remove only the selected extinction
term. These timings help locate work;
their deltas are not additive exclusive costs or quality-preserving speedups.

`LUMA_HAZE_WORK_COUNTS=1` selects a separate diagnostic compute entry point.
Each 8×4 workgroup reports eight sums followed by eight per-pixel maxima:
candidate visits, positive ray/cone intersections, shadow traversal segments,
depth reads, lit intervals, quadrature taps, shadow rays and first-block
whole-span lit proofs. The JSON records the final `relight` probe's camera,
which has moved 20 cm from the timed frozen pose. Padded edge lanes contribute
zero. These are operation counts; mean-to-maximum ratios describe work
distribution, not measured hardware occupancy. They do not count shared-grid
or surface work. Normal rendering retains its original compute entry point
and does not execute the counter reduction. The exact pixel gate still
applies; `performanceReferenceEligible: false` prevents treating diagnostic
timings as an optimization result. The API returns no counts after haze-off
or fragment-haze frames.

Live calls use `LIVE_SUBFRAMES` (currently two). Deterministic transport is evaluated once and does not blend historical density; stochastic transport retains its sample budget.
The compute pipeline specializes away MIS fallback, stochastic light selection
and unused jitter. An unmapped scattering source selects the generic fragment
path even when compute is requested; gobos also retain their existing path.
The counter API returns no counts for these fallback frames.

`LUMA_PROFILE_REPEAT` accepts `scene`, `medium-cache`, `fog-prepare`,
`fog-classify`, `fog-grid`, `fog-integrate`, or `haze-compute`. It executes the
selected pass twice when that pass is active. With `LUMA_PROFILE_DETAIL=1`,
each copy has its own timestamp bracket; the completion-cut endpoints belong
to the last copy. Scene copies clear and redraw the same attachments; compute
copies overwrite the same outputs using unchanged inputs. Unknown names fail
at pipeline creation. These runs remain quality-eligible but are explicitly
performance-ineligible. Compare whole-frame deltas to nearby controls: cache
behavior and overlap mean these deltas are not additive exclusive costs or
the savings available from deleting a pass.

Live captures warm for 64 frames with physical time frozen but temporal sample
seeds advancing. Timings are the final 32 warm frames. Eight subsequent captures
measure residual temporal variance. `first.png` shows cold history. `pan.png`
translates eye and target 20 cm over eight frames and is compared to the same
pose in reference captures. `blackout.png` and `relight.png` switch the volumetric
sources as a history diagnostic; fixture emissive materials stay pinned, so these
are **not** screenshots of a full score blackout. No artificial performance
claim should be inferred from these frozen-source timings.

`LUMA_FOG_BLOCK_STATS=1` explicitly copies the existing shared-volume classification
masks after `still-7`, outside all timed frames. `fog-blocks.json` records the
camera, grid/block dimensions and each block's candidate/wholly-visible light
counts. These are conservative candidates; they are not actual illuminated-cell
counts or hardware occupancy. `Renderer::fog_block_stats()` returns `None` before
classification and after a frame that did not classify. Normal rendering adds
no counter shader or readback.

GPU timestamps exclude GPUI layout, picking, presentation and readback waits;
they are not live FPS measurements. Replay the full saved score separately when
judging frame budget or moving/cue-changing lighting.
Each measured frame also records `wall_ms` around the complete blocking profile
call. It includes CPU submission, the full GPU queue, pixel/query readback and
completion, so work moved outside the GPU timestamp bracket remains visible.
It is an offscreen latency measurement, not displayed FPS.

Detailed GPU pass brackets are opt-in with `LUMA_PROFILE_DETAIL=1`; they
overlap and must not be summed. The Mac Gasworks profiling notes are in
`harness/perf/mac-gasworks-2026-09-09/PROFILING.md`.
The expensive fragment-candidate counter runs only when `fragment_stats()`
is explicitly requested, after the frame. `haze-lab` does not request it;
`profile-volumetrics` requests it once in sixteen measured frames, outside
their timestamp brackets. Its counts describe the original ray-mask/Z-bin
candidates, not MSAA invocations or the refined surface mask.
`LUMA_FOG_BLOCKS=0` disables both the conservative classification dispatch and
its use by grid lighting, so the diagnostic includes the work actually removed.
Exhaustive offline captures may explicitly set `LUMA_READBACK_TIMEOUT_SECS`
(30–600) when a laptop exceeds the default 30-second readback watchdog.

## CPU-only visibility reuse survey

`haze-lab inspect-reuse SUITE OUTPUT_DIR` expands the same pinned saved-score
states as `replay`, without constructing a GPU or opening a window. It archives
the inputs and writes `reuse.json` with exact f32-bit comparisons of the camera,
lighting bounds, radial grid extent, opaque depth geometry and fixture-shadow
casters. Cone geometry is matched as a multiset across compaction; source or
sorted indices must never be used as persistent light identity.

Counts are potential reuse candidates, not measured cache hits, saved GPU work
or FPS. Grid positions can change when active lights alter the lighting bounds,
even if almost every shadow map remains resident. Changing depth can expose
previously uncomputed grid cells. An implementation must track cell validity
and actual shadow-map contents, preserve current light addition order, and
recompute density/lighting at the current time. Prototype caches must still
pass the same-frame pixel and temporal audit.
