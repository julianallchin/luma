# wgpu renderer

Replacement for the three.js / react-three-fiber / drei / @react-three/postprocessing
stack, driven by the migration from Tauri+WebView to GPUI. One renderer serves the
live visualizer, the stage builder, and offline video export.

Status: design. Nothing here is implemented.

---

## 0. What exists today, and what that constrains

Read this section before arguing with any decision below.

**The eval engine is already the right shape.** `src-tauri/src/eval/scene.rs`:

```rust
pub struct Scene { pub annotations: Vec<CompiledAnnotation> }  // z-ascending
pub enum Scope { Single(usize), Composite }
impl Scene {
    pub fn render(&self, times: &[f32], scope: Scope, scratch: &mut Arena) -> Vec<UniverseState>;
}
```

It is pure in `t` (absolute track seconds), stateless across frames, rate-agnostic,
and seek-safe — "any frame can be the first frame computed, with no warmup"
(`docs/eval-ir.md`). A dense `times` grid is a bake; a one-element grid is a live
frame. This is exactly what both a 60 Hz viewport and a deterministic 60 fps export
need, and it needs no changes.

Its output is `UniverseState { primitives: HashMap<String, PrimitiveState> }` with

```rust
pub struct PrimitiveState { dimmer: f32, color: [f32;3], strobe: f32, position: [f32;2], speed: f32 }
```

Keys are `"fixture-uuid"` or `"fixture-uuid:head"`. `position` is `[pan_deg, tilt_deg]`.
There is **no beam angle, zoom, gobo, or focus** in the state, and the frozen
`Capability` enum is `{Color, Dimmer, Position, Strobe, Speed}`. Cone geometry is not
scene state and never will be — it is a property of the luminaire.

**Rust has no graphics code at all.** No `wgpu`, `glam`, `nalgebra`, `gltf`, or `image`
in any of the four `Cargo.toml`s. All 3D lives in TS. `src-tauri/src/fixtures/layout.rs`
is the only spatial Rust: `compute_head_offsets` and `head_world_position`, the latter
documented as "the single source of truth for how a primitive maps to a 3D point" —
and it reproduces three.js's `Rx(rot_x)·Ry(rot_z)·Rz(rot_y)` Euler composition with a
Y↔Z swap in and back out, because storage is Z-up and three.js is Y-up.

**There is no video export.** The task brief describes a "render-target → ffmpeg pipe"
path to unify with. It does not exist, in Rust or TS. ffmpeg is bundled
(`src-tauri/src/ffmpeg_env.rs::ffmpeg_path()`) but every caller is audio-only decode
or transcode. The nearest precedent for "dense time grid → pixel buffer" is
`src-tauri/src/annotation_preview.rs`, which steps an explicit time grid and emits raw
RGBA. Export is greenfield, which is good news: there is no legacy path to unify with,
only a shape to match.

**The waveform/DSP "render" path is unrelated.** `services/waveforms.rs` computes
envelope arrays; `track-editor/utils/timeline-drawing.ts` paints them to a 2D canvas.
No GPU, no overlap.

**Smells found while surveying, to be resolved as part of this work** (all of these
are the kind CLAUDE.md says to flag rather than extend):

1. **Two drifted cone tables for one concept.** `effects/volumetric-haze.tsx`
   `LUMINAIRES` says `moving_head: 26°`, `scanner: 22°`, `par: 70°`, `strobe: 100°`.
   `components/static-fixture.tsx` `BEAM_CONFIG` says `moving_head: 22°`,
   `scanner: 18°`, `par: 90°`, `strobe: 70°`. Same physical property, two answers.
   Meanwhile `Physical.Lens.degrees_min/degrees_max` **is parsed** into
   `models/fixtures.rs` from the QLC+ `.qxf` and is read by nobody.
2. **The mesh beam cones are dead code.** Both `<FixtureGroup>` mount sites
   (`stage-visualizer.tsx:1029`, `track-editor/components/pattern-preview.tsx:122`)
   pass `hideBeams`. The ~200 lines of `BEAM_CONFIG` + `BEAM_VERTEX`/`BEAM_FRAGMENT`
   cone shaders in `static-fixture.tsx` never render. Do not port them; delete them.
3. **A dual universe-transport path, half of it dead.**
   `stores/universe-state-store.ts` listens to both `universe-buffer` (batched,
   timestamped, interpolated) and `universe-state-update` (marked "legacy
   single-frame"). Nothing in Rust ever emits `universe-buffer`. The live path is the
   legacy one: a full `UniverseState` HashMap JSON-serialized and emitted at
   **240 Hz** (`render_engine.rs`, `sleep(4ms)`).
4. **`orbit-state.ts` is not camera state.** It is a window-level capture-phase
   pointer-drag tracker that exists solely because drei's `OrbitControls` calls
   `setPointerCapture` and thereby makes R3F fire a synthetic click at the end of an
   orbit drag. It is a workaround for a library we are deleting.
5. **The Y-up / Z-up swap is smeared across the codebase.** DB and eval are Z-up;
   three.js is Y-up. The swap appears in `use-stage-piece-store.ts::dataPoseFromWorld`,
   `stage-piece-node.tsx` (`position={[posX, posZ, posY]}`), `volumetric-haze.tsx`
   (`_euler.set(fixture.rotX, fixture.rotZ, fixture.rotY)`), `static-fixture.tsx`,
   `procedural-fixture.tsx`, and inside `head_world_position` on the Rust side.

---

## 1. Crate layout

Four new workspace crates under `src-tauri/crates/`. The dependency edges are a
strict DAG and each edge is load-bearing: the split exists so the math and the editor
logic are testable with no GPU and no window.

| Crate | Depends on | Purpose |
|---|---|---|
| `luma-assets` | `gltf`, `glam` | GLB/glTF parse → CPU meshes, materials, textures, AABBs. No GPU. |
| `luma-scene` | `luma-assets`, `glam` | Retained scene graph, transforms, camera, BVH + raycast, sockets, snap solver, gizmo state machine, orbit state machine. **No GPU, no gpui.** |
| `luma-render` | `luma-scene`, `wgpu` | Device, pipelines, passes, the haze chain, render targets, export stepping. No windowing. |
| `luma-viewport` | `luma-render`, `gpui` | The GPUI element: compositing, frame pacing, resize, input routing. |

`luma-scene` is where every existing pure-TS module lands: `snap.ts`, `sockets.ts`,
`tree.ts`, `mesh-cache.ts`, `stage-meshes.ts`, `model-scaling.ts`, and the parts of
`orbit-state.ts` worth keeping (none). Its test suite is the direct port of
`snap.test.ts` (20 cases, all synthetic, no GLB loading) plus new gizmo and orbit
state-machine tests. This crate can be finished and merged before a single triangle is
drawn — see phasing.

Math library: **`glam`, everywhere, no exceptions.** Do not pull `parry3d` for
raycasting; it drags in `nalgebra` and a second math vocabulary, which is the exact
"second way to do something" CLAUDE.md forbids. The BVH we need is ~200 lines (see §5).

Pin the gpui git dependency by rev. `harness/gpui/Cargo.toml` currently depends on
`gpui`/`gpui_platform` from `github.com/zed-industries/zed` with **no `rev =`** — it
resolves to `32a0e813a5132ee66b2cbc47d64b4c36b409f7f3` (gpui 0.2.2) today and will
silently move tomorrow. Fix this before building anything on it.

---

## 2. Renderer core

### 2.1 Coordinate convention — Z-up, and only Z-up

**Decision: the renderer is Z-up, right-handed, matching the database and the eval
engine.** Y-up dies with three.js.

This deletes: the swap in `dataPoseFromWorld`, the swap at every `<group position>`
site, the swap in every beam-direction computation, and the in-and-back-out swap
inside `head_world_position`. Nothing persisted changes — the DB is already Z-up, and
it was always three.js that was the odd one out. This is the single highest-value
cleanup in the port, and it is only cheap if it is done at the start.

Depth: reverse-Z with an infinite far plane (`clip.z ∈ [1,0]`, `Depth32Float`,
`CompareFunction::Greater`). The stage spans ~0.1 m to ~50 m; reverse-Z removes any
precision question for free and costs one matrix constant. Note this changes the raw
depth values that the haze pass and the bilateral composite read (`s.a` in
`haze-composite-effect.ts`) — the depth-difference sigma must be re-tuned in linear
view-space depth rather than raw NDC depth. Store **linear view depth** in the haze
target's alpha rather than raw depth; it makes the bilateral weight physically
meaningful (`uDepthSigma` in metres) instead of an NDC-curve artifact. That is a
deliberate, documented improvement over the current shader, not an accidental drift —
expect goldens on silhouette edges to differ slightly and accept it.

Rotation storage stays Euler XYZ radians in the DB. `luma-scene` must contain an
exact port of three.js `Euler.setFromQuaternion(q, "XYZ")`, including its clamping at
gimbal lock, with a round-trip test over every existing `stage_pieces` and
`patched_fixtures` row in `projects/*.luma`. A silent mismatch here corrupts saved
venues, and it will not look like a rendering bug when it happens.

### 2.2 Scene representation

Retained, flat, arena-indexed. Not an ECS — there is no gameplay, no systems, no
queries beyond "draw everything" and "raycast".

```rust
// luma-scene
pub struct NodeId(u32);

pub struct Node {
    pub parent: Option<NodeId>,
    pub local: Transform,              // { translation: Vec3, rotation: Quat, scale: Vec3 }
    pub world: Mat4,                   // cached, recomputed on dirty
    pub content: NodeContent,
    pub flags: NodeFlags,              // VISIBLE | PICKABLE | CASTS_SHADOW | RECEIVES_SHADOW
}

pub enum NodeContent {
    Empty,
    Mesh { mesh: MeshHandle, material: MaterialHandle },
    Emitter(EmitterId),                // a light-emitting fixture head; see §3
}

pub struct SceneGraph {
    nodes: Vec<Node>,
    dirty: BitVec,
    order: Vec<NodeId>,                // topological, parents before children
}
impl SceneGraph {
    pub fn update_world_transforms(&mut self);   // single pass over `order`
    pub fn world(&self, id: NodeId) -> Mat4;
}
```

`update_world_transforms` runs once per frame before anything reads `world`. This
replaces the current arrangement where three.js's live `Group.matrixWorld` **is** the
source of truth and everything (`piece-refs.ts`, the snap solver, the gizmo, marquee
selection) reads and mutates it mid-drag. That arrangement is why `piece-refs.ts` is a
global `Map<pieceId, THREE.Group>`. Here, `luma-scene` owns the authoritative
transforms and `piece-refs` disappears.

Scene content is rebuilt from the venue model (`PatchedFixture[]`, `StagePiece[]`)
whenever the venue changes, not per frame. Per frame, only emitter parameters and
gizmo/selection nodes change.

### 2.3 Materials — a closed set of four

One canonical way per surface class. There is no shader graph, no material
uber-shader with 40 feature flags, and no user-authored materials.

| Material | Used by | Shading |
|---|---|---|
| `Pbr` | fixture GLBs, stage-piece GLBs | metallic-roughness, base-color + optional texture, one directional light + up to 16 spotlights, one shadow map |
| `Unlit` | selection wireframes, gizmo handles, movement pyramids, circle-fit and mirror debug | flat colour, optional depth-test-off |
| `Grid` | the fading floor grid | the existing `GRID_FRAGMENT` shader, ported verbatim (world-XY analytic grid, `fwidth` antialiasing, distance fade) |
| `Ghost` | the placement preview | `Pbr` with `opacity 0.45`, `depth_write false`, emissive `#facc15` at `0.25` — exactly the current values in `stage-ghost.tsx` |

`Ghost` is deliberately its own material rather than a flag on `Pbr`, because it is
the only translucent surface in the app and it needs its own pass ordering. Two
opaque-pass materials plus two special cases is the whole vocabulary.

glTF import note: the `stage_lab` GLBs ship with only `POSITION` and `TEXCOORD_0` —
**no `NORMAL` attribute** — and three.js's `computeVertexNormals()` is currently
called per geometry to compensate (`stage-piece-object.tsx`). `luma-assets` must do
the same (area-weighted face normals accumulated to vertices, matching three's
implementation) or every stage piece will render black.

### 2.4 Camera

```rust
pub struct Camera {
    pub target: Vec3, pub radius: f32, pub azimuth: f32, pub polar: f32,
    pub fov_y_deg: f32, pub znear: f32,        // infinite far
}
```

Spherical, because that is what the orbit controller manipulates and what should be
persisted. `use-camera-store.ts` currently stores `position` + `target` and is **not**
persisted to disk (no zustand `persist` middleware) so it resets each launch;
spherical params should be persisted per venue as part of this work.

### 2.5 Frame structure

```
update_world_transforms
  ↓
shadow pass        → Depth32Float 4096²  (directional light only, light-stage mode only)
  ↓
scene pass         → Rgba16Float + Depth32Float, MSAA 4x, resolved
  ↓                   (opaque Pbr/Grid → then Ghost/Unlit-transparent, back-to-front)
haze pass          → Rgba16Float at resolutionScale, rgb = scatter, a = linear view depth
  ↓
temporal pass      → ping-pong Rgba16Float (live only; export uses subframe accumulation, §6)
  ↓
composite+tonemap  → the swapchain-equivalent Rgba8UnormSrgb / export target
  ↓
overlay pass       → gizmo, selection wireframes, marquee rect (Unlit, depth-test off, no tonemap)
```

Two departures from the current chain, both deliberate:

- **Composite and tonemap are one pass.** Today `HazeCompositeEffect` and the AgX
  `ToneMapping` effect are two `postprocessing` `Effect`s that the library merges into
  one `EffectPass` anyway. Without that library, writing them as two full-screen
  passes would be strictly worse. One WGSL fragment shader: bilateral-upsample the
  haze, add, tonemap, write.
- **The gizmo and selection overlays draw after tonemapping.** Today they go through
  AgX along with everything else, which means the `#facc15` selection yellow is not
  actually `#facc15` on screen. Drawing UI-coloured geometry after the display
  transform is correct and makes the overlay colours match the CSS ladder. Expect
  goldens on selection outlines to differ; that difference is a fix.

**Bloom is dropped.** `use-render-settings-store.ts` defaults `bloom: false`, and the
AgX + HDR-core approach in the haze shader (`uWhiteLeak`, `uBeamGain`, "white-hot is
emergent") was explicitly designed so bloom is not needed. Porting
`postprocessing`'s Bloom to WGSL to serve an off-by-default toggle is debt. Remove the
setting.

---

## 3. The signature effects

### 3.1 Volumetric haze — ports essentially 1:1

`effects/volumetric-haze-pass.ts` is the crown jewel and it is unusually portable: it
is one self-contained full-screen fragment shader with no scene-colour input, no
library dependencies beyond `Pass`, and no three.js types in its hot path. The whole
algorithm (analytic ray ∩ cone∩sphere span, equiangular + uniform MIS sampling,
Henyey-Greenstein phase, GDTF-style Gaussian angular profile, interleaved-gradient
noise with a golden-ratio temporal walk, closed-form `exp(-σt)` transmittance) is
GLSL→WGSL transliteration.

Four mechanical changes, each an improvement:

1. **Light data becomes a storage buffer, not a `DataTexture`.** The current
   `MAX_LIGHTS=256 × 16 floats` packed as 4 RGBA texels per light exists because WebGL
   has no SSBOs. In WGSL:

   ```wgsl
   struct LightCore { pos: vec3<f32>, range: f32 };                    // the reject read
   struct LightRest { dir: vec3<f32>, cos_beam: f32,
                      color: vec3<f32>, intensity: f32,
                      cos_field: f32, wash: f32, _pad: vec2<f32> };
   @group(1) @binding(0) var<storage, read> light_core: array<LightCore>;
   @group(1) @binding(1) var<storage, read> light_rest: array<LightRest>;
   ```

   Structure-of-arrays specifically to preserve the property the current texel layout
   was designed for: *a pixel a light does not reach costs one 16-byte read, not four*.
   That is what makes the 256-light loop viable and it must survive the port.

2. **It also removes a WGSL hazard.** WGSL forbids `textureSample` in non-uniform
   control flow; the current shader does `lightTexel(...)` inside a data-dependent
   loop with `break` and `continue`. With storage buffers there is no sampling in the
   loop at all, so the problem never arises. (The one depth read stays at the top of
   `main`, uniform.)

3. **Alpha carries linear view depth, not raw NDC depth** (see §2.1), so the
   bilateral sigma in the composite is in metres.

4. **The angular profile stays a function, and stays the gobo seam.** The comment in
   `angularProfile` — "replace it with a texture lookup in cone-local polar
   coordinates to project arbitrary gobo shapes without touching any call site" — is
   the correct seam and should be preserved verbatim in the WGSL. When gobos arrive,
   `cos_field`/`cos_beam` grow a `gobo: u32` index and the function grows a texture
   array lookup. Nothing else changes.

Defaults to carry over exactly: `beamGain 180`, `whiteLeak 0.03`, `phaseG 0.6`,
`nearClamp 0.06`, `sigma = density * 0.06`, ambient fill `vec3(0.014, 0.011, 0.009)`
over 8 stratified taps to `min(hitDist, 24)`, `MAX_SAMPLES 32`, default 8.

**Does not port:** the keyboard tuning dials (`` ` ``, `[`, `]`, `;`, `'`, `,`, `.`)
in `volumetric-haze.tsx`. They were for dialling in a look and the values are now
baked. Debug modes 0–3 (full / no-noise / no-lights / passthrough) should port, as a
render-settings enum rather than a hidden keybind, because the goldens use them (§7).

### 3.2 Emitters — where cone geometry comes from

Today `volumetric-haze.tsx` does per frame, in TypeScript, for every fixture:
resolve the model kind, look up a hardcoded `LUMINAIRES` entry, derive
`cosBeam`/`cosField`/`range`/`wash`/`gain` from a solid-angle concentration formula,
compose pan/tilt/fixture-euler quaternions into a beam direction, gate on strobe
phase, and normalise pixel-bar intensity by `sqrt(headCount)`.

The brief says the renderer consumes fixture states and never computes them. That is
right about *state* and wrong about *geometry*: none of the above is scene state, it
is luminaire physics, and it belongs next to `head_world_position` in
`src-tauri/src/fixtures/layout.rs`, which is already declared the single source of
truth for primitive→3D-point mapping. Split it:

**Moves into `luma` (`src-tauri/src/fixtures/`), beside `head_world_position`:**

```rust
pub struct Luminaire { pub field_angle_deg: f32, pub lumens: f32 }
pub fn luminaire_for(def: &FixtureDefinition, mode: &Mode) -> Luminaire;
pub struct Cone { pub cos_beam: f32, pub cos_field: f32, pub range: f32, pub wash: f32, pub gain: f32 }
pub fn cone_from_opening(l: &Luminaire) -> Cone;              // port of coneFromOpening
pub fn beam_axis(rot: [f64;3], pan_deg: f32, tilt_deg: f32) -> Vec3;
```

`luminaire_for` is where smell #1 gets fixed: read `Physical.Lens.degrees_min/max`
from the parsed `.qxf` (already there, currently read by nobody) and fall back to the
per-`FixtureType` table only when the definition omits it. One table, one answer, fed
by real fixture data. `FixtureType::detect` already exists in `models/groups.rs` —
use it, do not add a third type-classification path.

**Stays renderer-side (`luma-render`):** strobe phase gating. `PrimitiveState.strobe`
is a 0..1 rate, not a boolean; turning it into on/off requires the display clock and
is a per-frame visual concern. Keep the existing mapping (`hz = strobe * 20` for
beams, `* 10` for pixel bars, 50% duty) but note it is currently *two different
constants for one concept* and should be unified to one in the port.

Per frame, `luma-render` walks the emitter list and fills the two storage buffers.
That loop is ~50 lines of arithmetic over pre-resolved data, with no hash lookups —
compared to today's per-frame `resolveFixture` + `definitionsCache.get` + string-keyed
`getPrimitive(\`${id}:${h}\`)` per fixture per frame.

### 3.3 Spotlight pool

`lib/spotlight-pool.ts` is a fixed pool of 16 `THREE.SpotLight`s, re-assigned each
frame to the brightest 16 requests. In wgpu it is a 16-element uniform array in the
scene-pass bind group and the same "sort by intensity, take 16" selection, which moves
into `luma-render` next to the emitter loop. The pool no longer needs to exist as a
mutable global attached to a scene — it is just the top-16 slice.

**Parity hazard.** `light.penumbra = 0.6`, `light.decay = 1.5` (physical is 2.0), and
three's `SpotLight` distance/angle attenuation must be ported *verbatim*, not
"physically". `decay = 1.5` is a deliberate non-physical look. If the WGSL uses a
textbook inverse-square, every lit surface in every golden shifts and the failure will
be diffuse and hard to localise. Port `getDistanceAttenuation` and the
`smoothstep(1, penumbraCos, angleCos)` term from three's `lights_pars_begin.glsl.js`
and unit-test the 1D falloff curve against sampled three.js values before rendering
any scene.

Fixture spotlights do not cast shadows and must not start to — the comment in
`spotlight-pool.ts` records that three's per-frame shadow-camera rebuild (driven by
`angle`/`distance` changing every frame) made the brightest cones vanish entirely.

### 3.4 Shadows

Smaller than it looks. Exactly one shadow caster exists: the single `directionalLight`
in `stage-visualizer.tsx`, mounted only when `!darkStage`, 4096², orthographic
`[-15,15]²`, near 0.5 far 60, `normalBias 0.01`, `PCFSoftShadowMap`. No cascades, no
point/spot shadows, static light direction. One depth pass, one PCF kernel. Port the
3×3 PCF from three's `shadowmap_pars_fragment.glsl.js` for parity.

### 3.5 Mapping table

| Today | wgpu | Notes |
|---|---|---|
| `VolumetricHazePass` | full-screen WGSL fragment | 1:1; `DataTexture` → SoA storage buffers |
| `HazeTemporalPass` | ping-pong `Rgba16Float` | 1:1; live only, export uses §6 subframes |
| `HazeCompositeEffect` | merged into composite+tonemap pass | bilateral upsample, now in metres |
| `ToneMapping(AGX)` | same pass | **must match `postprocessing`'s AgX exactly** |
| `Bloom` | — | dropped (off by default; see §2.5) |
| `EffectComposer multisampling={4}` | MSAA 4x on scene pass | haze/composite are non-MSAA |
| `frameBufferType={HalfFloatType}` | `Rgba16Float` | |
| spotlight pool (16 `SpotLight`) | 16-element uniform array | port three's attenuation verbatim |
| `directionalLight` + `PCFSoftShadowMap` | one 4096² depth pass + 3×3 PCF | |
| `FadingGrid` `ShaderMaterial` | `Grid` material | verbatim, XY instead of XZ |
| `static-fixture.tsx` beam cones | — | **dead at every call site; delete** |
| `<Bloom>`/`<ToneMapping>` React glue | explicit pass list | no library, no JSX |

**Nothing in the effect chain fails to port.** The risk is not capability, it is
numerical parity of AgX and of three's light attenuation.

---

## 4. GPUI integration

### 4.1 The problem

gpui at rev `32a0e81` offers no zero-copy RGBA texture handoff. Concretely:

- `gpui::surface(CVPixelBuffer)` / `Window::paint_surface` exists but
  `gpui_apple/src/metal_renderer.rs:1118` hard-asserts
  `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`. It is a **video path**: NV12 YUV
  only, macOS-only, and the wgpu backend drops surface primitives entirely
  (`gpui_wgpu/src/wgpu_renderer.rs:1531` — `PrimitiveBatch::Surfaces(_) => {}`).
- gpui does not expose its device or queue. `Window::gpu_specs()` returns strings.
  On macOS gpui is raw Metal, not wgpu, so we would be on a different device anyway.
- `canvas()` and a custom `Element` only let you emit gpui's own scene primitives.
  There is no "give me this frame's command encoder" hook.

So the only portable route is: render offscreen with our own wgpu device,
`copy_texture_to_buffer`, wrap the BGRA bytes in `gpui::RenderImage`, and draw it with
`img()`.

**Do not use the NV12 surface path.** Chroma subsampling on a view whose content is
1-pixel gizmo axes, thin beam edges, and saturated-colour cones is a correctness
regression dressed as a performance win.

### 4.2 The plan

**v1 — readback.** Own `wgpu::Instance` → offscreen `Rgba8UnormSrgb` at
`bounds.size * window.scale_factor()` → `copy_texture_to_buffer` into a 3-deep staging
ring → map buffer from frame N−2 → `RenderImage::new(Frame)` → `img(image)` inside a
`div()` that carries the input handlers → `window.drop_image(frame_n_minus_3)`.
`RenderImage` gets `scale_factor` set to the window's, or `img()` draws it at 2×.

Be honest about the cost: 1600×1000 at 2× DPI is 3200×2000×4 B = 25.6 MB per frame,
1.5 GB/s at 60 Hz, plus two frames of added latency (~33 ms). That is tolerable for a
stage-builder viewport and marginal for a live show view synced to audio.

Mitigations available immediately, before any gpui surgery: the haze target already
has `resolutionScale`; the *composite* target can have one too, capping presentation
at 1× DPI (12.8 MB/frame) with the scene still rendered at 2× and downsampled. The 3D
view is a haze-dominated image, not text.

**v2 — a BGRA surface path in gpui.** The right fix is ~100 lines in
`gpui_apple/src/metal_renderer.rs`: a second pipeline that samples one BGRA texture
from a `CVMetalTextureCache` alongside the existing NV12 one, and a
`SurfaceSource::Bgra` variant. We render into an IOSurface-backed `MTLTexture` (via
`wgpu-hal`'s Metal interop), wrap it as a `CVPixelBuffer`, and hand it to
`paint_surface`. Zero copy, zero added latency.

This means carrying a gpui patch. That is real debt and should be taken with eyes
open: pin the rev (which we must do anyway), keep the patch to a single additive
commit on a fork, and open the upstream PR — it is a generally useful feature and
`gpui_wgpu` would want the mirror-image change. Ship v1 first; only take the patch if
measurement says readback is the bottleneck.

Do **not** pursue the third option (a sibling `NSView` with its own `CAMetalLayer`
composited by AppKit outside gpui's scene). It wins on paint cost and loses on
everything else: z-order against gpui overlays, clipping inside a dock panel, input
routing, and it is macOS-only forever.

### 4.3 Element, pacing, resize, input

```rust
// luma-viewport
pub struct Viewport { renderer: Rc<RefCell<Renderer>>, scene: Entity<SceneModel>, ... }

impl Render for Viewport {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();           // vsync via CVDisplayLink
        div().size_full()
            .child(img(self.latest_frame.clone()).size_full())
            .on_mouse_down(MouseButton::Left, ...)
            .on_scroll_wheel(...)
    }
}
```

- **Pacing:** `window.request_animation_frame()` at the top of `render` is the
  canonical continuous-redraw idiom (`gpui/examples/paths_bench.rs:58`). It is
  vsync-paced by CVDisplayLink on macOS.
- **Resize:** the platform resize triggers `window.refresh()`, so `render` runs with
  new `bounds`. Compare against the cached size and reallocate targets in `prepaint`,
  not `paint`. Reallocating every render target on every frame of a panel drag is the
  obvious trap; debounce reallocation by one frame and letterbox in between.
- **Input:** `on_mouse_down` / `on_mouse_move` / `on_scroll_wheel` on the `div` for
  the simple cases; for drag capture (keep receiving moves after the cursor leaves the
  viewport — mandatory for both orbit and gizmo drags) use `window.insert_hitbox` in
  `prepaint` plus paint-phase `window.on_mouse_event::<MouseMoveEvent>` /
  `::<MouseUpEvent>`. Those are cleared every frame and must be re-registered in each
  `paint`. `MouseMoveEvent` carries no delta — track the previous position.
  `ScrollDelta` is `Pixels` or `Lines` (trackpad vs wheel); handle both or the mouse
  wheel will feel broken.
- Because we own input routing end to end, the entire class of bug that
  `orbit-state.ts` works around does not exist. Delete it.

**The 240 Hz JSON firehose goes away.** The renderer is in-process with the eval
engine, so at paint time it calls
`scene.render(&[audio_time], Scope::Composite, &mut arena)` directly and gets a
`UniverseState` with no serialisation, no IPC, no interpolation store, no 240 Hz
timer. `universe-state-update`, the dead `universe-buffer` path, and
`universe-state-store.ts`'s `findFrames` interpolation all delete. ArtNet output keeps
its own ~44 Hz timer in `render_engine.rs` and is unaffected — it never needed to be
coupled to the visualizer's rate, and after this it isn't.

A per-frame `Arena` is reused, as `render_engine.rs` already does.

---

## 5. Editor tooling

The most underestimated part of the port, and the least glamorous. Budget accordingly:
this is more code than the haze.

### 5.1 Picking — CPU raycast against a BVH, not a picking buffer

**Decision: CPU raycast. No GPU picking buffer.** Three reasons, in order:

1. A picking buffer cannot serve the case that matters most. `stage-ghost.tsx` and
   `unified-transform.tsx::surfaceUnderPoint` cast a **downward** ray `(0,-1,0)` from a
   world point, with no camera involved, to find the floor surface under a dragged
   piece. That is a query, not a click, and a screen-space ID buffer has no answer.
2. The snap solver needs `hit.face_normal` and the owning node's world matrix, plus a
   depth-sorted hit list. An ID buffer gives you one id and one depth.
3. We would therefore need CPU raycast regardless — and shipping both is exactly the
   "second way to do something" the design rules forbid.

Scene scale makes this easy: tens of pieces, ~19 MB of GLB total, and the GLBs are low
poly. Implementation in `luma-scene`:

```rust
pub struct MeshBvh { nodes: Vec<BvhNode>, tri_indices: Vec<u32> }   // binned SAH, built once per mesh at load
pub struct RayHit { pub node: NodeId, pub t: f32, pub point: Vec3, pub face_normal: Vec3, pub tri: u32 }
impl SceneGraph {
    pub fn raycast(&self, ray: Ray, filter: PickFilter) -> Vec<RayHit>;  // sorted by t
}
```

Per-mesh BVH built at asset load, cached alongside the mesh in `luma-assets`. Per
raycast, transform the ray into each candidate's local space (cheaper than
transforming the BVH), test the node AABB first, then descend. A top-level BVH over
node AABBs is not needed at this scene size and should not be built until profiling
asks for it.

Selection semantics to preserve: multi-select `Set<NodeId>` plus a `last_selected`
primary that drives gizmo pivot and the bright outline; shift-click toggles; plain
click replaces and clears the cross-type selection. Marquee selection stays
**projection-based, not raycast** — project each piece's world origin to screen space
and rect-test, with a 5 px minimum drag, exactly as today.

### 5.2 Transform gizmo — an explicit state machine

drei's `<TransformControls>` (~1100 lines of three-stdlib) is the single largest thing
being reimplemented. Specify it rather than discovering it:

```rust
pub enum GizmoHandle {
    TranslateAxis(Axis), TranslatePlane(Axis),   // Axis = normal of the plane
    TranslateScreen,
    RotateAxis(Axis), RotateScreen,
}
pub enum GizmoState {
    Idle,
    Hover(GizmoHandle),
    Dragging { handle: GizmoHandle, frame: DragFrame, targets: Vec<DragTarget> },
}
struct DragFrame { plane_point: Vec3, plane_normal: Vec3, grab_offset: Vec3 }
struct DragTarget { node: NodeId, start_world_pos: Vec3, start_world_rot: Quat, start_anchor: Vec3 }
```

- **Handle picking is analytic, not mesh-based.** three uses invisible fat cylinders
  and planes as picker geometry; we have a dozen handles with closed forms — ray ∩
  capsule for axes, ray ∩ disc for plane quads, ray ∩ torus (or ray ∩ plane then an
  annulus radius test, which is simpler and adequate) for rotate rings. No BVH, no
  proxy meshes, tested before scene geometry, ~150 lines and exactly testable.
- **Constant screen size:** scale the gizmo by
  `dist_to_camera * tan(fov_y/2) * k`, matching drei's `size={0.5}`.
- **Drag plane selection.** For `TranslateAxis(a)`: the plane containing `a` whose
  normal is the one of the two candidates more facing the camera. For
  `TranslatePlane(n)`: that plane. For screen handles: the camera-facing plane through
  the pivot. On grab, store the hit point; each move, ray∩plane and (for axis handles)
  project the delta onto the axis.
- **Edge-on culling:** hide an axis handle when `|dot(axis_view, view_dir)| > 0.99`,
  because the drag math degenerates there. three does this; users notice when it's
  missing.
- **Delta application** ports directly from `unified-transform.tsx`, including its
  good idea: the gizmo attaches to an **empty pivot node**, not to the selection, and
  `on_change` reads the pivot delta and rewrites it onto every target. That is what
  lets one widget drive a mixed selection of fixtures and stage pieces, and it should
  survive verbatim. Translate: `start + delta_pos`. Rotate/individual: rotate the
  origin→anchor offset by `delta_q` about the target's own anchor,
  `new_q = delta_q * start_q`. Rotate/group: same, pivoting about the drag-start pivot
  position. `rotation_snap = 15°`.
- **Anchor heuristics** port as-is: parented pieces pivot at their inferred attachment
  socket; free pieces pivot at bbox **bottom-centre**, because stage GLB origins are
  usually at a corner.
- Modes are `Translate | Rotate` only. There is no scale gizmo and there should not
  be — `StagePiece.scale` is a single uniform float driven by physical dimensions.

### 5.3 Orbit camera

Port the semantics, not the library: LMB rotate, MMB dolly, RMB pan, `zoom_speed 0.5`,
**damping off**. With damping off it is a pure state machine — no per-frame
integration, no settling — and therefore directly unit-testable: feed a sequence of
`(button, position, modifiers)` events, assert the resulting spherical params. Do that;
camera feel regressions are otherwise invisible until someone complains.

Add the polar clamp three defaults to (`ε .. π−ε`) and keep no azimuth or distance
limits, matching today.

### 5.4 Snapping

`snap.ts` (482 lines) and `sockets.ts` port to `luma-scene` with **no design change**.
The algorithm is pure math with a shallow three.js surface — `Vector3`/`Quaternion`/
`Matrix4` → `glam::Vec3`/`Quat`/`Mat4`, `Box3` → an AABB struct — and, critically, the
raycast is *not* inside it: the caller pre-computes a `SnapSurface` and passes it in.
That is a clean seam and it is already the right one.

Port every constant: `ATTACH_THRESHOLD 0.5 m`, `EDGE_OUTWARD_THRESHOLD -0.3`,
parallel-normal test `0.9`, twist epsilon `1e-8`, `derivePerpendicular` guard `0.99`,
`TRUSS_INSET 0.15`, `CABLE_COVER_END_INSET 0.005`,
`SPEAKER_STAND_POLE_OFFSET [0.1,0,0]`, surface-normal upward test `dot > 0.7`.

Port the 20 tests in `lib/__tests__/snap.test.ts` first, as Rust tests. They are fully
synthetic (hand-written `ResolvedSocket` arrays, a `Map`-backed `lookup_sockets`
injection, no GLB loading), so they port nearly line for line and they validate the
math layer before any rendering exists. This is the single best-value first commit in
the whole project.

Two hazards:

- three.js `Matrix4` is column-major with row-major *constructor* arguments;
  `makeBasis` sets columns; `transformDirection` is upper-3×3 multiply then normalise.
  Get these wrong and the tests will tell you, which is the point.
- The module docstrings in `sockets.ts` and `snap.ts` claim edge mode is a 180°
  rotation about the host normal. `flipFor()` returns `EDGE_IDENTITY`. **Port the
  code, not the prose**, and fix the docstrings on the way through.

The socket type vocabulary (13 types) and the `COMPATIBLE` adjacency table are
authored data that currently live only in TS while `parent_piece_id` and the resolved
transform live in Rust. Move them to Rust with the solver — the split is what makes
this a TS-only feature today.

### 5.5 Ghosts

`stage-ghost.tsx` ports to a `Ghost`-material node driven by the same per-frame
`solve_snap`. Keep the subtle bit its comments record: measure the AABB from a
**freshly loaded** mesh, not from the mounted ghost node, because the mounted one has
already been moved by the snap solver and measuring it would bake a world offset into
the idempotent mesh cache permanently.

---

## 6. Video export

One renderer, two render targets. `Renderer::render_frame` takes a target and knows
nothing about where it goes.

```rust
pub enum RenderTarget {
    Live { color: wgpu::Texture, readback: StagingRing },
    Offscreen { color: wgpu::Texture, readback: wgpu::Buffer },   // export, arbitrary resolution
}
pub struct FrameRequest {
    pub time_sec: f32,
    pub frame_index: u32,          // drives the haze jitter; deterministic
    pub camera: Camera,
    pub overlays: bool,            // false for export — no gizmo, no grid, no selection
}
```

Export is a loop, and it is deterministic because every input is:

```rust
for n in 0..total_frames {
    let t = start + n as f32 / fps;
    let states = scene.render(&[t], Scope::Composite, &mut arena)[0];   // pure in t
    renderer.render_frame(FrameRequest { time_sec: t, frame_index: n, .. }, &target);
    ffmpeg_stdin.write_all(&readback)?;
}
```

`Scene::render` is already pure and seek-safe, so the eval side needs nothing. The
three stateful things in the renderer are the temporal haze accumulator, the
`uFrame` jitter counter, and `uElapsed` (haze noise drift). Handle them:

- `uElapsed = t`. The noise field is a function of world position and time; feeding
  track time makes the drift identical on every run.
- `uFrame = frame_index`. Deterministic per output frame.
- **The temporal pass is skipped entirely in export.** Instead, accumulate `K`
  subframes at the *same* `t` with `uFrame = n*K + k`, averaging in the haze target.
  This is strictly better than the live EMA (no ghost trails, no motion guard, no
  convergence lag) and it is the *same primitive* — the jitter index — used
  differently rather than a second denoiser. `K = 16` should be visually noise-free;
  it is a quality/time dial with no other consequence.

Frames pipe as raw `rgba` to the already-bundled ffmpeg via
`src-tauri/src/ffmpeg_env.rs::ffmpeg_path()`:
`ffmpeg -f rawvideo -pix_fmt rgba -s WxH -r FPS -i - -c:v ... out.mp4`, plus the
track audio as a second input. Reuse the existing stdin-pipe pattern from
`src/sync/files.rs`.

The time-grid construction (`preview_times`, `STEPS_PER_BEAT`, the width clamp) in
`annotation_preview.rs` is the same idea one level down. Extract the shared helper
rather than writing a second time-stepper; that file is the existing precedent for
"dense time axis → pixel buffer" and it should not end up as the odd one out.

---

## 7. Verification

### 7.1 Capture the goldens now

**Before any three.js code is touched**, including the cone-table unification in §3.2.
A golden captured after a look change is a golden of the wrong thing.

Extend the existing harness pattern (`harness/README.md`, `harness/shot-web.mjs` —
Playwright WebKit against a Vite page, 2× device pixels, per-fixture ids). Add:

- `harness-3d.html` + `src/harness/scenes-3d.tsx` — mounts `<StageVisualizer>` with a
  fixed venue, a fixed camera pose, and fixed render settings.
- `harness/shot-visualizer.mjs` — the capture driver.
- Output to `harness/shots/three/<scene>@<t>.png`, later
  `harness/shots/wgpu/<scene>@<t>.png`.

**The determinism seam already exists.** `PrimitiveOverrideContext`
(`hooks/use-primitive-state.ts`) lets a scene inject a fixed
`(id) => PrimitiveState` with no eval engine, no Tauri, and no IPC. Every fixture and
the haze pass already read through it. Use it — do not build a second injection path.

Three sources of nondeterminism must be pinned in the harness:

1. `state.clock.getElapsedTime()` drives haze noise drift and strobe phase → mount the
   `<Canvas>` with `frameloop="never"` and drive `advance(t)` manually with an
   explicit clock.
2. `uFrame` drives the jitter walk → deterministic once frames are advanced manually
   from a fresh mount.
3. The temporal EMA needs warmup → advance **64 frames at the same `t`** before
   capturing, so the accumulator has converged. (This is the live equivalent of the
   export subframe loop in §6, which is a useful consistency check on both.)

### 7.2 Golden scenes

Eight scenes × three timestamps `{0.0, 1.37, 4.20}` (irrational-ish offsets so no
strobe or noise phase lands on a special value).

| # | Scene | Isolates |
|---|---|---|
| 1 | one moving head, tilt 30°, white, haze on | beam axis, cone half-angle, near-field core |
| 2 | 8-mover fan, saturated R/G/B/magenta, overlapping cones | colour, overlap summation, per-light jitter decorrelation |
| 3 | par wash onto floor + a truss occluding the beam | occlusion, bilateral upsample at silhouettes |
| 4 | LED bar, procedural pixels, per-pixel colours | procedural path, `sqrt(headCount)` normalisation |
| 5 | stage builder: lit stage, shadows, grid, selection outline, gizmo | PBR, shadow map, grid shader, overlay pass |
| 6 | strobe at 0.5, captured mid-duty and mid-gap | strobe phase gating |
| 7 | full venue, **haze density 0** | pure geometry/material/tonemap baseline |
| 8 | 120 fixtures, all on, dense haze | perf + the 256-light path |

Scene 7 is the one to make pass first: it has no stochastic content at all, so any
difference is a real bug in geometry, materials, lighting, or tonemap.

### 7.3 Comparison — three metrics, not one

SSIM alone is the wrong tool here: it will fail on grain that does not matter and pass
on a 3° beam-axis error that does. Use all three, per scene, with per-scene thresholds:

1. **Structural.** SSIM on the pair after a σ=4 px Gaussian blur. Kills stochastic
   grain, keeps structure. Threshold ≥ 0.98 for haze scenes, ≥ 0.995 for scene 7.
2. **Colour.** Oklab ΔE on 16×16 block means. Catches tonemap and attenuation drift,
   which is the failure mode most likely to be systematic and least likely to be
   visible in SSIM. `src-tauri/src/node_graph/oklab.rs` already exists — use it.
   Threshold: mean ΔE ≤ 0.01, p99 ≤ 0.03.
3. **Analytic probes**, per scene, asserted numerically:
   - beam centroid direction and 50%-intensity half-angle, from a threshold+moment fit
     on the haze buffer (scenes 1, 2);
   - floor light-pool ellipse centre and axes (scenes 1, 3);
   - a binary "haze present" mask compared against the depth-edge mask (scene 3);
   - shadow edge position along a fixed scanline (scene 5);
   - rendered pixel position of each fixture origin, projected (all scenes).

**Acceptable differences** — do not chase these:
- haze grain pattern (different RNG stream; the IGN + golden-ratio walk will not
  reproduce bit-identically and does not need to);
- temporal convergence residual after 64 frames;
- MSAA edge coverage dithering, ±1 subpixel;
- selection/gizmo overlay colours, which **change by design** (§2.5 — they no longer
  pass through AgX);
- silhouette-adjacent haze within ~2 px, from the linear-depth bilateral change (§2.1);
- font rendering anywhere in the frame.

**Bugs** — any of these fails the port:
- beam axis off by more than 0.5°, or half-angle off by more than 2%;
- mid-beam hue shift beyond the ΔE threshold (the near-field core is *expected* to
  clip to white; sample the ring at a fixed distance, not the core);
- occlusion wrong: haze visible past geometry, or beams cut short;
- a fixture, piece, or shadow in the wrong place;
- a light missing from the top-16 spotlight selection.

### 7.4 Per-frame budgets

Target 1600×1000 at 2× DPI on Apple silicon, 60 Hz, with 64 active emitters:

| Stage | Budget |
|---|---|
| `Scene::render` (CPU, 200 primitives, 1 frame) | ≤ 1.0 ms |
| emitter fill + world-transform update (CPU) | ≤ 0.3 ms |
| shadow pass | ≤ 0.5 ms |
| scene pass (MSAA 4×) | ≤ 2.0 ms |
| haze pass (half-res, 8 samples, 64 lights) | ≤ 4.0 ms |
| temporal + composite + tonemap | ≤ 0.5 ms |
| overlay pass | ≤ 0.2 ms |
| present (v1 readback) | ≤ 3.0 ms |
| **total** | **≤ 11.5 ms**, leaving headroom for 120 Hz |

Scene 8 (120 fixtures, dense haze) is the stress case and may exceed the haze budget;
`resolutionScale` and sample count are the dials. `gpui-component`'s `crates/fps`
(`gpui-fps`) is a ready-made frame-time/GPU/memory HUD and should be wired into the
viewport from phase 1 — measure from the first frame, not after the first complaint.

`Scene::render` timing must actually be measured, not assumed; it is currently invoked
at 240 Hz on a background thread and its per-frame cost is not recorded anywhere.

---

## 8. Phasing

The walking skeleton is **phase 1**: a GPUI window showing a wgpu-cleared colour that
resizes, repaints at vsync, and reports its frame time. It contains no renderer and it
proves the one thing nothing else can be built on top of.

| Phase | Deliverable | Validated by |
|---|---|---|
| **0** | Goldens captured from three.js. `luma-assets` + `luma-scene`: glTF load, AABB, transforms, Euler XYZ three-compat, sockets, snap solver. **No GPU.** | The 20 ported `snap.test.ts` cases; an Euler round-trip test over every row in `projects/*.luma` |
| **1** | `luma-viewport` skeleton: gpui element, own wgpu device, clear colour, readback→`RenderImage`→`img()`, `request_animation_frame`, resize, `gpui-fps` HUD | It runs at vsync and survives a panel drag; readback cost measured against the §7.4 budget |
| **2** | Geometry: glTF meshes on GPU, `Pbr` + `Unlit` + `Grid`, directional light + shadow map, orbit camera, reverse-Z | **Golden scene 7 passes** (haze off — pure geometry/material/tonemap) |
| **3** | Haze: emitter fill, spotlight array, analytic haze pass, temporal, composite+AgX | **Golden scenes 1–4, 6, 8 pass.** Cutover point: three.js can be deleted after this |
| **4** | Editor: BVH raycast, selection, gizmo state machine, ghost, snap wired to drags, marquee | **Golden scene 5 passes**; gizmo and orbit state-machine unit tests |
| **5** | Export: offscreen target, subframe accumulation, ffmpeg pipe | Byte-identical output across two runs of the same export |

Sequencing notes:

- **Phase 0 is entirely off the critical path of the GPUI migration** and has no
  dependency on wgpu, gpui, or any decision in §4. Start it immediately; it is also
  the highest-confidence work in the project.
- **Do the parity spikes before phase 2, not during it.** Two isolated tests, each an
  afternoon: (a) AgX applied to a 0..8 HDR ramp in WGSL vs `postprocessing`'s AgX,
  compared numerically; (b) three's spotlight `getDistanceAttenuation(decay=1.5)` +
  penumbra falloff vs the WGSL port, over a 1D distance and angle sweep. If either is
  off, every subsequent golden fails at once and diffusely. Nail them standalone.
- **Do not delete the three.js path until phase 3 goldens pass.** It is the reference.
- The eval-engine cleanups (§4.3 — deleting the 240 Hz emit, the dead
  `universe-buffer` path, and the interpolation store) land with phase 3, when the
  in-process renderer becomes the only consumer.
- The cone-table unification (§3.2) is a **three.js-side change made before phase 0
  goldens are captured**, so the goldens record the intended look rather than a look we
  intend to change. It is the only pre-port change to the existing renderer.

---

## 9. Open questions

1. **Is the 3D view macOS-only for v1?** It decides whether the gpui BGRA-surface
   patch (§4.2 v2) is worth taking, or whether we live with readback for portability.
2. **Are we willing to carry a gpui fork** for that patch, and to open the upstream PR?
3. **Confirm Z-up everywhere** (§2.1). No persisted data changes; it deletes five
   scattered swaps and a Y↔Z round trip inside `head_world_position`.
4. **Confirm bloom is dropped** (§2.5). Off by default today.
5. **Should cone angle come from `Physical.Lens.degrees_min/max`** (real per-fixture
   data, currently parsed and unread) instead of the two drifted hardcoded tables? This
   changes the look of every existing venue. Recommendation: yes, and do it *before*
   golden capture.
6. **Export spec**: target resolution, fps, codec/container, and confirmation that
   overlays (grid, gizmo, selection) are off — presumably yes.
7. **Is 2 frames (~33 ms) of viewport latency acceptable** for the live show view
   under v1 readback? It is fine for the stage builder; it is a judgement call for a
   view someone watches next to the real rig.
