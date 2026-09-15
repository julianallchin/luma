# wgpu renderer

One renderer serves the live stage view, the stage builder, offline goldens and
score recording. This doc is an overview. The module docs are the detailed
contract.

## 1. Crates

| Crate | Path | Purpose |
|---|---|---|
| `luma-scene` | `gpui/crates/scene` | Scene math and editor logic. No GPU, no window. Scene graph, camera, framing, BVH raycast, sockets and the snap solver, gizmo, selection, the venue graph resolver, patch allocation and distribute. |
| `luma-render` | `gpui/crates/render` | Offscreen wgpu rendering for the stage and waveform strips. No GPUI dependency. |
| `luma-app` | `gpui/crates/app` | The GPUI element that shows frames (`visualizer.rs`, `stage/`), input, and the View settings card. |

The backend uses `luma-render` too: `backend/src/stage_render.rs` renders one venue at
one moment, and `backend/src/recording.rs` records a score to video.

## 2. Coordinates

Renderer space is Z-up, right-handed, `f32`. It matches the database and the eval
engine. The socket layer (`sockets.rs`, `snap.rs`) is asset space: Y-up, `f64`,
because glTF is Y-up and the solver goldens need `f64` precision. The conversion
happens at the scene boundary. See the `luma-scene` crate doc.

## 3. Frame

`frame::build` turns a `scene_desc::Scene`, fixture definitions and evaluated
fixture states into a `Frame`. `Renderer::render(frame, w, h, subframes)` draws it.
The main parts are:

- **Scene pass.** PBR surfaces with 4× MSAA (`MSAA_SAMPLES`), HDR environment
  lighting (`environment.rs`) and a physically based sky (`atmosphere.rs`). AgX
  tonemapping. There is no bloom.
- **Light index.** Screen tiles plus Z-bins, built in compute
  (`light_index.rs`). See `docs/design/light-index-unification.md`.
- **Shadows.** Sun cascades, and per-fixture shadow maps (`shadow.rs`). By default
  every active emitter keeps a map, up to 512 layers.
- **Haze.** Analytic beam transport in `shaders/beam_transport.wgsl`, a baked
  density field (`haze_field.rs`), a shared far-field grid (`fog_grid.rs`), and the
  lit-interval cache (`interval_cache.rs`).
- **Overlay.** Gizmos, selection and socket marks (`overlay.rs`).

`docs/design/volumetrics-v2.md` has the phase status of the volumetric work.
`docs/design/stage-rendering-quality.md` has the quality targets and measurements.

## 4. Presentation

`viewport.rs` drives the renderer at interactive rates on its own thread.
`AsyncViewport` holds a bounded ring of slots with nonblocking submit and take.
On Metal the frame is written into an `IOSurface` that the GPUI compositor samples
without a copy (`share.rs`). Where the platform cannot share memory, pixels are read
back off the UI thread. See `docs/design/presentation-seam.md`.

Frame pacing, resize debouncing and input belong to the GPUI element, not to the
renderer.

## 5. Editor tooling

Picking is a CPU raycast against a per-mesh BVH (`luma-scene/src/bvh.rs`). There is
no GPU picking buffer, because the snap solver needs face normals and downward
surface queries with no camera. The gizmo is an explicit state machine
(`gizmo.rs`). Snapping and sockets are in `snap.rs` and `sockets.rs`. The venue
graph is in `venue.rs`; see `docs/design/venue-graph.md`.

## 6. Export

Built. `luma-record <score-id> <out.mp4>` (`backend/src/bin/luma-record.rs`) records
one score. Offscreen frames accumulate `DEFAULT_SUBFRAMES` jitter samples at the
same time with the temporal pass bypassed, so output is deterministic. See
`docs/design/score-video.md`.

Future work: an app dialog, a Python binding, and recording many scores in one run.

## 7. Verification

- `render-goldens` writes `harness/goldens/scenes-wgpu/`. `--check` writes nothing
  and exits non-zero on drift.
- `render-contract-goldens` writes `gpui/crates/render/goldens/contracts/`. Each PNG
  has a sidecar with its full scene.
- `profile-volumetrics` measures GPU and CPU frame cost.
- `haze-lab` pins haze quality experiments. See
  `gpui/crates/render/experiments/haze-lab.md`.

## 8. Future work

- Geometry-aware indirect lighting. There is no GI.
- Shadow tiers and moment maps (`docs/design/shadows-phase3.md`).
- Calibrated fixture photometry. `luminaire.rs` reads lens angles from the fixture
  definition where present and uses relative per-kind lumen budgets.
