# Stage rendering quality and performance

The target is Depence visual quality and Unreal-class realism at interactive
frame rates. This doc holds that target, what the renderer does toward it, and
what is not built. It is not a claim that the renderer meets the target.

The haze path has its own doc, [haze-lit-interval-cache.md](haze-lit-interval-cache.md).

## Acceptance

Hardware, output resolution, fixture count and frame-rate target must be stated
together for any performance claim. A timing on one GPU says nothing about
another.

Review a complete venue with materials and geometry, not just cones against
black. Pin camera, light output, exposure, medium density and source state.
Explicitly enable surface lighting and fixture shadows.

The acceptance suite needs:

- A white room with a red surface: indirect red spill, occluded corners, no light through a separating wall.
- Black drapes, rough stage decks, metallic truss and reflective fixtures under the same lights.
- One calibrated fixture at several distances; matching surface illuminance and volumetric radiance.
- Hundreds of moving heads, dense overlapping beams, close cameras, beams aimed at the eye, and near-plane crossings.
- Blackout, strobes and fast RGB changes in motion. Reuse must not leave glowing history after a light turns off.
- Locked deterministic stills and a higher-sample reference render, plus live recordings of the same scene. A pleasing still does not prove correct motion or frame pacing.

Measure GPU stages separately, CPU submission, memory, and end-to-end
presentation p50/p95/max. Never describe correctness at 512 fixtures as a
performance measurement.

## What is not built

- **Geometry-aware indirect lighting.** There is no pass that bounces fixture
  light off surfaces with wall occlusion. Surfaces get their surroundings from
  the relit reflection probes (`probes.rs`) and the haze's glow (below). Do not
  add an emitter-summed ambient fill to stand in for it: it lights surfaces
  even when beams hit nothing.
- **Fixture photometry.** `gpui/crates/scene/src/luminaire.rs` uses relative
  lumen outputs (1 is a stock moving-head lamp) and a shared beam gain. A
  quoted zoom range is represented by its midpoint, not by live optics.
- **Calibrated throw.** A cone's integration range is 24–60 m depending on
  concentration (`luminaire.rs`), with a soft range taper. This is an
  approximation, not a calibrated physical throw.

## External calibration

Epic documents a 4 ms Lumen budget for the 60 fps console tier and 8 ms for the 30 fps tier at 1080p internal resolution, with temporal upsampling for higher output resolutions. Its surface lighting cache and radiance cache explicitly bound update work. These are examples of measured architecture budgets, not promises for this engine or hardware:
https://dev.epicgames.com/documentation/en-us/unreal-engine/lumen-performance-guide-for-unreal-engine

Syncronorm describes physically based light beams and PBR materials as part of Depence's lighting renderer. Use the rendered result as a quality reference without assuming its proprietary implementation:
https://www.syncronorm.com/products/depence2/visualization/lighting/page

## Fixture shadows

`RenderSettings::fixture_shadows` turns on shadows from opaque venue geometry
in fixture light and haze. `RenderSettings::geometry_shadows` keeps a retained
shadow map for every fixture instead of a 16-map budget
(`MAX_FIXTURE_SHADOWS` in `shadow.rs`). The constructors in `scene_desc.rs`
start with it off; the stage (`visualizer/render_controls.rs`), a recording's
show scene (`backend/src/stage_render.rs`) and `luma-cli profile` all set it
on. The app has no control for it.

With it on:

- Each active emitter keeps a 256-square depth map (`FIXTURE_SHADOW_SIZE`).
  The allocation grows in powers of two with the cone count, up to 512 layers
  (`MAX_FIXTURE_CONES`). 512 layers cost 128 MiB; small rigs allocate less.
- A map's identity is its projection plus the opaque geometry that casts into
  it (`ShadowCacheKey`). Colour, intensity, blackout and light-list reordering
  do not redraw maps. A stage edit invalidates the maps it affects.
- Fixture housings do not cast into fixture shadows. Transparent objects do
  not cast.
- A moved head keeps its slot and its last map. At most 64 moved maps
  (Medium: 16) are redrawn per frame, ranked by priority times frames waited
  (`budget_redraws`). A slot holding another light's map is always drawn.
- Dark cones (intensity times brightest channel at most 1e-4) get no shadow
  map and no beam work.
- Surfaces and haze share the maps.

Low quality draws no fixture shadows at all (see "Quality levels").

`visibility.rs` holds a software triangle BVH over the opaque stage geometry.
It is a reference path that only renderer tests turn on
(`visibility_reference`). It is too slow for interactive use: it traces a
world-triangle query for every contributing light and integration sample.

Sun cascades are keyed by every opaque draw they rasterise, fixture bodies
included, so a moved head does not leave its old sun shadow. The cascade
normal offset follows each surface's N·L, `0.002 + (r + 1.5) · texel · sin θ`,
and the depth bias is in metres along the light, `0.002 + texel / 2`. Limit: in
the second cascade (from 12 m, about 4 cm texels), the 3×3 filter still lets a
little sun into the first 10 cm under a flat occluder.

## Haze

At High and Medium the haze is marched by compute kernels, and every live
frame and capture is deterministic. `Renderer::render_reference` is the
exhaustive per-ray reference.

- **Shared far field.** Each beam's light past its handover is shared in a
  camera-relative radial grid (`fog_grid.rs`, `shaders/haze_grid.wgsl`,
  `shaders/haze_integrate.wgsl`): 8-pixel cells at High and 16 at Medium, and
  128 quadratic distance slices. Every contributing light is evaluated in each
  cell; there is no light selection. A parallel prefix scan integrates density
  and camera extinction along each column, and a haze pixel reads its
  integrated lighting at scene depth.
- **Source region.** A beam stays per-pixel from its source to its handover
  (`fog_grid::handover`) and is the grid's past 10/7 of that. The two weights
  sum to one; this is not a shorter beam range. The light index carries a
  second mask plane for these source regions.
- **Handover.** The grid blurs a beam by about a cell, so it takes a beam
  where the steepest part of its profile would need 0.55 m to fall from the
  peak to nothing: the softness of a 60° cell at 1.75 m (`SOURCE_INNER`), the
  case the distances were measured on. A 50° bar or par is handed over at
  1.75 m like a wash, a 20° spot at about 8 m and an 8° beam at about 21 m.
  There is no beam angle at which a light changes path: a rule by angle sent
  a 50° ten-lamp bar down the per-pixel path whole, at over ten times the
  cost of a 60° one.
- **Gobos, and beams still sharp at their range,** are integrated per ray.
- **Plain-beam piece cap.** Below cloudiness 0.5, `lit_interval` uses at most
  8 pieces (Medium: 4), never fewer than the equiangular criterion needs
  (`quadrature_pieces`).
- **Memory.** Grid textures are allocated on first use and retained across
  blackouts.

The grid is a quality and performance tradeoff, not exact transport: grid
interpolation and finite depth quadrature are approximations. Near-source
detail, gobo transport, surface lights and shadow-map resolution are
preserved.

Low marches nothing and draws each beam in closed form (see "Quality levels").

## Sky visibility and ambient occlusion

The ambient terms (house ambient, probe diffuse and probe specular) are scaled
by a per-pixel visibility texture (`sky_visibility.rs`,
`shaders/ambient_occlusion.wgsl`), so the legs and underside of a deck do not
get the full sky and the full ground bounce. Sun and fixture light are not
changed; they have their own shadows.

- **Height field (outdoors).** The stage is rendered from straight above and
  from straight below, without the ground plane and without fixture bodies.
  Each texel keeps one slab: its lowest and highest surface. The texel is the
  stage footprint over 1024, and never less than 5 cm (5 cm up to a 51 m
  stage, about 10 cm at 100 m). The field is redrawn only when the stage
  geometry changes, with the fixture maps' caster key. A pixel marches 8
  azimuths × 12 radii. Standing geometry raises the horizon; an overhang hides
  a band of elevations. The rest is weighted by the cosine lobe and scales the
  sky part of the probe irradiance. The point is lifted one texel plus 2 cm
  along its normal before the march; after that the height compares use a
  5 mm margin that counts a wall touching an overhang as under it. A surface
  facing straight down has no sky lobe and gets zero.
- **Ground bounce (outdoors).** The probe's lower hemisphere is split off as
  `E(-Z) (1 - n.z) / 2`. It is scaled by the sun and sky visibility of the
  ground under the point. That comes from a map at twice the field texel
  (sun marched through the slabs, box-filtered mips), read with nine taps over
  the lobe's footprint. The sun share uses the sun and sky irradiance on open
  ground.
- **GTAO (always).** Jimenez 2016 on the single-sample depth prepass: 3 slices
  × 4 steps per side, 0.75 m radius, thin-occluder compensation, normalised by
  the unoccluded arc. A 5×5 depth-aware box removes the per-pixel noise. It
  multiplies the house ambient and the probe (specular through Lagarde's
  specular occlusion), together with the material AO map.

RTX 5090, Vulkan, 1920×1080, 8×4 m deck test scene, median GPU time:
per-pixel visibility 0.29 ms outdoors (0.11 ms indoors, GTAO only), denoise
0.03 ms, scene pass +0.003 ms. On a stage edit: height maps 2×0.002 ms,
convert 0.004 ms, ground map 0.05 ms (also on a sun change).

Limits:

- The ground is the plane `z = 0`. Terrain, pits and raised audience floors are
  not ground for this model.
- One slab per texel. A deck under another deck, or the space between a roof
  and a stage under it, reads as solid. Sun under an overhang is marched
  through the slabs, so low sun reaching under a deck from the side is kept.
- Geometry outside the stage footprint is open sky. Nothing else occludes it.
- Overlapping overhang bands are added, not merged, and the radii are sampled,
  so the edge of an overhang can be off by one radius step.
- The ground map is blurred wider than the true lobe: near a deck edge the
  ground bounce is somewhat too bright (in the test scene the lit-ground share
  under the deck centre is about 0.2, against about 0.12 calculated).
- GTAO is screen space. It cannot see off-screen or hidden occluders, and it
  is evaluated per pixel, so MSAA edge samples get their pixel's value.
- Indoors only GTAO applies. A roof is not a reason to darken house ambient.

## Camera look: exposure, tone curve, glare

`RenderSettings::look` (`scene_desc::Look`) holds the tone curve, the
exposure and the glare. `Look::NEUTRAL` (fixed exposure, AgX, no glare) keeps
the single composite pass, and is the look the tracked images are captured
with. The live stage defaults to `Look::STAGE` and persists the look as the
device setting `stage_look`. Any look that needs more than the composite pass
runs `post.rs`:

- **Metering.** 128-bin log histogram of a quarter of the pixels,
  centre-weighted. The mean of the 60th–98th percentile of lit pixels is
  exposed to middle grey; the result is clamped to `photometry::ADAPT_EV`
  (−6 to +4 EV) before the compensation is added. Near-black pixels are not
  metered and a frame with almost nothing lit holds the previous exposure.
  Adaptation is an eye's: 1.2 /s opening; 3 /s closing, but only as far as
  a pupil closes (`photometry::PUPIL_STOPS`, 4 stops), and the rest over
  about a minute, so a light that arrives in the face is blinding until
  then. A standalone capture snaps.
- **Glare.** Exposed light above 2× diffuse white (soft knee) is
  area-averaged into a padded FFT grid and convolved with a glare kernel by
  FFT (`post_glare.wgsl`, wgpu compute), as Unreal's convolution bloom does.
  The whole frame is convolved, so an extended or oddly shaped bright area
  glares as its shape does (Ritschel et al. 2009); there is no sprite per
  lens and no bloom pyramid. The grid is 1024 × 512 at High and 512 × 256 at
  Medium and Low; the frame fills at most half of each side (455 × 256 texels
  for 16:9 at High, about 4 px each at 1080p and 5.6 px at 1440p), so the
  kernel reaches across the whole frame without the cyclic convolution
  folding it back. 1024 complex texels is also the most one row transform
  holds in the default 16 KiB of workgroup memory. Both inputs are real, so
  rows keep only their non-negative frequencies; red and green share one
  complex transform. A frame is three dispatches (hot pixels + row FFT,
  column FFT × kernel spectrum × inverse column FFT, inverse row FFT into the
  glare texture). The kernel's spectrum is built again only when the style,
  the diffraction amount, or the grid's focal length or padding (field of
  view, aspect) change by more than 1%: 15 ms of CPU on 32 cores, about 85 ms
  on one, plus two dispatches.

  The kernel (`psf.rs`) is energy per texel as a fraction of the source's
  light, converted from angle by the camera's field of view:
  - **Veil.** Vos's glare spread function (Vos 1984, as used by Spencer et
    al. 1995 and Yoshida et al. 2008), normalised in closed form over 30°:
    `0.384·2.61e6·exp(−(θ/0.02)²) + 0.478·20.91/(θ+0.02)³ + 0.138·72.37/(θ+0.02)²`.
    Its 1/θ² tail reaches across the frame. About 26% of a source's light
    lands outside its own texel at High, 1080p, 50°. Bloom and eye use it
    whole; iris and star use a tenth of it, a camera's veiling glare being a
    few percent (ISO 9358) with the same wide additive shape (Qian et al.,
    CVPR 2026).
  - **Aperture pattern** (iris, eye, star). |F{A·exp(iφ)}|² baked once at
    startup on a background thread: a 512² FFT per 40 nm band (8 bands, each
    with its own defocus phase, 0.15 waves at the rim), resampled at 4
    wavelengths each, 32 from 395 to 705 nm, weighted into linear sRGB by the
    CIE 1931 observer (Wyman et al. 2013 fit, negative lobes clipped). After
    Wu et al., "How to Train Neural Networks for Flare Removal" (ICCV 2021),
    the lens carries seeded dust (N(30, 5²) dots, radius 0.4–2.5% of the
    pupil, opacity U(0, 1)) and scratches (N(30, 5²) polylines of U(1, 16)
    segments, opacity U(0, 1)); the eye has a quarter as many beside its
    ciliary fibres and particles. Iris: nine rounded blades; star: six
    straight blades; eye: a round pupil. The 3 × 3 samples around frequency
    zero (the Airy core, a thousandth of a degree in a real lens, a tenth
    here) are cleared: that light stays with the source. What is left is
    16–20% of the light, the scratch streaks reaching ±25°.
  - **No ring.** There is no core fade and no `r / CORE` lift. The centre
    texel, the source's own, takes its four neighbours' mean, so the glare
    neither doubles the source nor dips under it. The kernel fades to zero
    only near the grid's padding and the pattern only near its own edge, both
    far out on the tail. The tonemap reads the glare through a cubic
    B-spline, smooth to the second derivative.

  `strength` scales the glare, which is otherwise physical. The lens is
  drawn at about 600× diffuse white (`LENS_GAIN`), nearer a real lamp's
  thousands, so lenses glare and reflections on truss, a few times white, do
  not. `star` weights the pattern; 0.5 is physical. Star is a hexagonal
  aperture's six diffraction rays. The glare is added after the tone curve,
  saturating at the display's white.

  Bake: 85 ms on 32 cores (190 ms on 4, 650 ms on 1), off the startup
  thread; the first frame with glare waits for it. GPU cost, RTX 5090 /
  Vulkan, Get Lucky at Gasworks at 146 s, min of five alternating 150-frame
  p50s, against glare off: 1920×1080 +0.11 ms; 2560×1440 +0.12–0.13 ms; the
  half-size grid +0.05 ms. The cost is set by the grid, not the frame, apart
  from the hot-pixel average.
- **Tone curves.** AgX (three's), AgX with Blender's Punchy look (the stage
  default), and Narkowicz's ACES fit. HDR expansion above the 0.6 knee applies
  to each.

## Quality levels

`RenderSettings::quality` is `High`, `Medium` or `Low`. The View popover's
Viewport section sets it; it is the `stage_quality` device setting
(`"high" | "medium" | "low"`). A machine that had set the older
`stage_low_quality` flag reads as `Medium` until a level is written. The
renderer turns the level into a `QualityBudget` (`quality.rs`), the one table
every pass reads.

- **High** is the reference picture. Its live viewport renders fewer pixels,
  never under the reduced levels' 1 MP, when the GPU's time keeps its frames
  from 30 a second (`DynamicBudget` in the app's `render_scale.rs`); the
  reduced levels hold 60 and 120 and have no pixels to give.
- **Medium** is a laptop GPU's budget: haze at half resolution at most, 4 gobo
  samples at most, a 1 MP pixel budget, MSAA 1×, 16 px fog cells, a plain-beam cap of 4 pieces, 16
  shadow redraws a frame, twice the shadow-walk footprint and half its steps,
  and twice again while the camera moves. It also runs no local reflection
  probes, coarser sun shafts, clouds and haze glow, the half-size glare grid,
  and no floor detail.
- **Low** keeps Medium's budgets and:
  - **Marches no haze** (`QualityBudget::marched`). No fog grid, medium
    cache, lit-interval cache, compaction or haze kernel runs, and there are
    no sun shafts and no haze glow. One compute pass,
    `shaders/haze_analytic.wgsl`, writes the haze target the composite
    already reads, at a quarter of the output resolution. For each light in
    the pixel's light-index list it integrates single scattering through
    uniform haze in closed form: in the equiangular angle about the cone's
    virtual apex, `dt / d²` is exactly `dθ / h`, so the inverse-square term
    integrates to `(θ₁ − θ₀) / h` over the span `beam_span` clips to the
    cone, and a fixed three-point Gauss rule in `θ` carries the slowly
    varying rest (angular profile, phase, range taper, camera and light
    transmittance at the mean extinction, the medium's envelope). Low draws
    no haze shadows, no gobos in the beam and no cloud density; the medium
    uniform's cloudiness is zero, so surfaces and the sky see the same
    uniform haze.
  - **Draws no fixture shadows** (`QualityBudget::fixture_shadows`). No shadow
    slot is assigned, so no map is drawn, redrawn or reduced into its
    hierarchy, the shadow atlas is not allocated, and surfaces take every
    fixture's light unshadowed. A beam aimed into a deck lights the floor
    under it. `Frame::fixture_shadows` can still turn them off at any level;
    the budget can only turn them off too.
  - **Estimates ambient visibility at half resolution**
    (`QualityBudget::ambient_divisor`). GTAO, the outdoor sky and ground
    visibility and the 5×5 denoise run once per 2×2 block, at the block's
    first pixel; the scene pass reads the texel covering its pixel.

The atmosphere's sky tables and the cloud history are keyed by the cloud
`Budget` they use, not by `Quality`, so a switch between Medium and Low (same
cloud budget) rebuilds nothing.

Gasworks Park rig with its 34 movers lit over 300 dimmed strip cells (344
cones), outdoor at night, haze 0.12, 2227×1391 output at every level (the
app's 1 MP pixel budget for Medium and Low is not applied here), M-series
Mac, median GPU frame time of 280 frames per level, levels interleaved in
one run. Other GPU work on the machine made the static wide case noisy; its
numbers are the cleanest of three runs:

| | static wide | static side | orbiting wide | orbiting side |
|---|---:|---:|---:|---:|
| High | 41.7 ms | 40.7 ms | 40.6 ms | 46.8 ms |
| Medium | 20.6 ms | 23.0 ms | 21.0 ms | 21.5 ms |
| Low | 11.7 ms | 11.2 ms | 10.1 ms | 10.7 ms |

Low's frame by pass (`LUMA_PROFILE_DETAIL=1`, brackets overlap, static):
haze-analytic about 4.7 ms (the frame with haze 0.12 against haze 0:
10.2 against 5.5 ms wide, 11.2 against 7.4 ms side), ambient visibility
1.0 ms and its denoise 0.6 ms, surface-light-index 0.9 ms, surface depth
0.7 ms, light index 0.6 ms, and the scene pass the rest, about 2 to 3 ms.
Post passes are each under 0.2 ms.

The analytic pass's cost is per (pixel, light) pair whose ray crosses the
cone, so a rig of many wide washes seen face-on is its worst case. At half
resolution it cost about 15 ms on the same frame, which is why Low draws it
at a quarter.

## Haze glow

A beam in haze is seen because the haze scatters some of its light toward the
camera. It scatters the rest every other way, and that light lights the room
and the haze round the beam, for every source alike: fixture cones and laser
beams and sheets (`haze_glow.rs`, `shaders/haze_glow*.wgsl`). With no haze
there is no glow, and its light comes from where the beams actually scatter,
not from the emitters' sum. It runs at High and Medium.

- **Inject.** A world grid over the medium's bounds (64×64×32 at High,
  32×32×16 at Medium). Each cell holds `σ · density · E`, the power its haze
  scatters per cubic metre, from every cone (one shadow-map tap at the cell,
  so a cone's light stops at what it hits) and every laser beam and sheet. A
  source narrower than a cell is spread over a cell's width keeping its power,
  so no beam falls between cells; laser sheets are merged into fans of at most
  0.06 rad first.
- **Spread.** The grid's mips, gathered (`haze_glow_gather.wgsl`). The
  fluence from scattering haze falls as `e^(−σr)/4πr²`; over a shell r..2r
  that is `S̄ r e^(−σr)`. Each cell steps six cones, one each way along each
  axis, out through shells r..2r, each read from the level as wide as the
  shell is deep. One fluence grid comes out, and every reader takes it in one
  place.
- **Occlusion** (`glow_occupancy.rs`). Every opaque surface but the
  fixtures' marks 4³ sub-cells per glow cell, on the CPU and only when the
  geometry or the grid changes. Each cell stores the share of its lines along
  x, y and z that a marked sub-cell stops; a coarser level joins two cells
  along the axis (stopped by either) and averages the pairs beside each
  other, so a thin wall blocks all of a coarse cell's lines across it and a
  deck lying flat blocks none along it. A cone takes the blocking along its
  own axis from the cells between its cell and the shell, a level finer than
  the shell and never its own cell, so a surface does not shade its open
  side. A laser beam's injection stops at the first marked sub-cell on its
  line; a merged sheet stops where its middle does.
- **Use.** Surfaces take a quarter of the fluence as irradiance, read as a
  cubic B-spline in eight filtered taps, so the cells do not show as
  creases where the glow fades; the composite marches 12 steps of the
  camera ray, one trilinear read each, and adds the haze's second
  scattering of it, `σ · density · φ`. The gather takes each cell's own
  haze over its neighbours, weighted 1 2 1, so a narrow beam's cells read as
  a line rather than a string of cell-sized lumps.

The glow seen grows about with σ² while the room is thinner than one mean
free path: three times the haze lit the floor of the laser test room 40 times
as much (`laser::haze_lit_by_lasers_lights_the_room`). For a fixture beam the
glow sits about `σw/2` under the beam's own radiance, eight stops at density
0.3, so it shows in thick haze and in a dark room, and barely otherwise.

The exposure meter ignores light more than six stops under the top of its
band (`SHOWN_STOPS` in `post_exposure.wgsl`), so the glow, black in the
picture, does not outweigh the beams.

Limits: six cones see in six directions, so geometry at 45° blocks less
cleanly than a wall along an axis, and an object narrower than a cone (a
speaker, a truss leg) blocks it only on the cone's axis. A truss marks the
sub-cells its tubes pass through, so it blocks more than its open lattice
would. The visible laser beam itself is depth-tested, not stopped: seen from
beyond a wall, it still shows past it. Indoors the haze ends at the room.

Under a 6 m deck below a fan of laser beams the haze glows a third as much
as with no deck (`laser::a_deck_shades_the_glow_under_it`).
