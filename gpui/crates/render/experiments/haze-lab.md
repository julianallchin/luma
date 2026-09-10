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
in the suite are renderer Z-up, unlike the legacy Y-up `Scene.camera` fields.
Catalogue paths in a hand-written suite resolve relative to that suite.

## Experiments

Each invocation starts a fresh process. Use a clean environment: externally set
`LUMA_*` renderer switches remain effective and are recorded in `capture.json`.
Keep each variant in its own output directory.

| Mode / switch | Change from live defaults |
| --- | --- |
| `live` | Native-resolution deterministic beams, shared far-field grid, production subframe budget |
| `uniform` | Cloudiness zero, to distinguish physical cloud variation |
| `no-grid` | Per-ray broad-light integration, reservoir selection retained |
| `exact-live` | Diagnostic MIS integration, every light evaluated at native resolution; 8 samples retained |
| `full-live` | Same native target as current live defaults |
| `reference16/32/64/128` | Native-resolution haze, every light, 32 integration samples, stated accumulated subframes, grid disabled |
| `LUMA_FOG_TILE_SIZE=4/8/16/32` | Diagnostic grid resolution sweep; default 8, fixed for process lifetime |
| `LUMA_FOG_BLOCKS=0` | Disable shared block visibility proofs; evaluate every candidate cell/light |
| `LUMA_FOG_DEPTH_CULL=0` | Retain all column prefixes, including those hidden behind geometry |
| `LUMA_VISIBILITY_REFERENCE=1 LUMA_GEOMETRY_SHADOW_SAMPLES=32` | Existing software triangle visibility instead of cached shadow maps; use with a reference mode |

The reference converges our current single-scattering model. It is not an
independent path-traced physical ground truth: source extinction still uses its
angular cache, density has finite quadrature, and accumulation uses half-float
render targets. Compare two reference sample counts before treating small image
differences as meaningful. A triangle-visibility reference additionally removes
the shadow-map approximation, at considerable cost for dense geometry.

Live calls use `LIVE_SUBFRAMES` (currently two). Deterministic transport is evaluated once and does not blend historical density; stochastic transport retains its sample budget.

Live captures warm for 64 frames with physical time frozen but temporal sample
seeds advancing. Timings are the final 32 warm frames. Eight subsequent captures
measure residual temporal variance. `first.png` shows cold history. `pan.png`
translates eye and target 20 cm over eight frames and is compared to the same
pose in reference captures. `blackout.png` and `relight.png` switch the volumetric
sources as a history diagnostic; fixture emissive materials stay pinned, so these
are **not** screenshots of a full score blackout. No artificial performance
claim should be inferred from these frozen-source timings.

```sh
python3 gpui/crates/render/experiments/compare_haze.py /tmp/haze reference live
```

Requires Pillow and NumPy. Outputs an HTML contact sheet and JSON metrics. RMSE
and temporal deviation are display RGB code values (0–255), not HDR radiance.
The tool rejects differing cameras or output sizes. Uniform-haze variants change
the physical scene and are labeled as such, not ranked as accuracy failures.
GPU timestamps exclude GPUI layout, picking, presentation and readback waits;
they are not live FPS measurements. Replay the full saved score separately when
judging frame budget or moving/cue-changing lighting.

The earliest frozen experiments accidentally used one subframe. Their timing tables are historical diagnostics, not equivalent to the production two-subframe path. Saved-score replays used the production budget throughout.
