# Volumetrics v2 — renderer architecture

Status: design, not implemented. Scope: `gpui/crates/render/` — volumetric beams, light
culling, fixture shadows, frame structure.

Reference hardware: Apple M3 Max, 1920×1080, wgpu 26.0.1 / Metal.
Workload: up to 512 moving-head cones, constantly panning/tilting, interactive orbit camera.

---

## 0. The one-sentence thesis

**Beams are geometry; haze is a volume.** Sharp cone edges come from rasterizing a cone hull
and integrating single-scattering in closed form along the ray's exact span inside it. Soft
ambient media comes from a low-resolution froxel grid with temporal reprojection. The two
systems partition the *light set*, never the medium, so every fixture in-scatters exactly
once and extinction has exactly one owner.

This is deliberately **not** "port the renderer to froxel volumetrics." The literature is
explicit that froxel grids blur exactly the feature this product sells. §2.1 makes that case
with citations; §3 is the architecture that follows from it.

---

## 1. Current state

> **Which tree this describes.** §1.1–1.3 were written against a *working tree* that already
> carried tactical fixes, not against `f86782f`. At `f86782f` **both** cullers had the
> `behind_eye ⇒ whole screen` branch — `clusters.rs::bounds_for` had it at line 360, exactly
> as `haze_tiles` did. Both have since been given near-plane-correct bounds by clipping the
> bound's twelve edges to the near plane and projecting the survivors, which is exact for a
> convex hull, and both now share that geometry through `clusters::box_corners` /
> `for_each_clipped_vertex` rather than keeping two copies of it. Read §1.3(a) and (b) as a
> description of the *class* of bug, not as a claim about which module was guilty.

### 1.1 What is already right, and must survive

`shaders/haze.wgsl` is not a naive per-pixel ray march, and the brief that prompted this doc
mischaracterised it. What it actually does, per candidate light:

1. Analytic ray∩sphere span against the light's finite range (`s0..s1`), clamped to scene
   depth so geometry occludes the beam.
2. Analytic ray∩cone quadratic, partitioning `[s0,s1]` at the cone roots and keeping the
   sub-interval whose midpoint is inside the forward cone. Both solids are convex, so the
   result is one contiguous span `[t_a, t_b]`.
3. Estimation of the single-scattering integral over that span with a **two-strategy MIS
   estimator**: half the samples equiangular (`t = δ + h·tan θ`), half uniform, combined
   with the balance heuristic.

That estimator is the correct primitive and it is what the literature recommends for this
exact problem. The equiangular substitution and its PDF are Kulla & Fajardo's
(EGSR 2012) — sampling proportional to the `1/(D²+t²)` geometry term is why 8 samples give
a clean beam where 64 uniform samples would not. Keeping it is non-negotiable; everything
below changes *where and how often* it runs, not the integrand.

Also already correct: reverse-Z handling, HDR with no clamp (white-hot core emerges from the
broadband leak plus tonemapper rather than a radiance gate), per-light jitter decorrelation,
and alpha carrying linear view depth for the composite's bilateral upsample.

### 1.2 The measured tail

From `goldens/volumetric-profile-m3-max.json` (600 frames, release, subframes=2,
haze_resolution 0.5, haze_steps 8):

| case | cones | gpu_volumetric p50 | p95 | **max** | max/p95 |
|---|---|---|---|---|---|
| transport-32 | 32 | 0.22 ms | 0.29 ms | 5.38 ms | 18.7× |
| transport-128 | 128 | 0.78 ms | 1.06 ms | 14.14 ms | 13.3× |
| transport-512 | 512 | 3.05 ms | 5.07 ms | **139.95 ms** | **27.6×** |
| fixture-shadows-120 | 120 | 0.14 ms | 1.22 ms | 14.77 ms | 12.1× |

Cluster stats, transport-512: **5,793,674 light references over 17,495 occupied clusters =
331 lights per cluster average**, i.e. 65 % of all 512 lights are in the average cluster.
Cold cluster build 13.8 ms on CPU. The grid is doing no work.

For calibration: DOOM (2016) ships a 16×8×24 = **3,072**-cluster grid and holds ≤256 lights
per cluster as a hard ceiling.¹ We have 5.7× more cells and 331 lights in the average one.
Cell count is not the problem.

> **These numbers came from a static benchmark and understate the problem.** The camera and
> every cone were held still, so the cluster cache hit on all but 1–4 of 600 frames and
> `cpu_cluster` was recorded as exactly `0.0` in all four cases — the profiler measured only
> the cache-hit frame. Under an animated benchmark the grid rebuilds *every* frame (720
> rebuilds per run) and the real cost appears: `cpu_cluster` p95 of 0.9 / 3.6 / 15.9 / 2.8 ms
> across the four cases. Cell *count* is still not the problem; cell *shape* is (Phase 1).

### 1.3 Root causes, precisely

**(a) `clusters.rs::bounds_for` bounds a cone with a world-space AABB, then fills a box.**
It computes a per-axis radial extent around the cone's base disc, unions with the apex,
projects the 8 corners (with correct near-plane edge clipping — this part is right, and is
better than the brief assumed), and then **fills every cluster in
`[x0..x1]×[y0..y1]×[z0..z1]` unconditionally**. There is no cone-vs-cluster test anywhere.
A 15° cone of 30 m throw pointed diagonally has an AABB of roughly 30³ m; the projection of
that box is enormously larger than the beam's true screen footprint, and every cell inside
the projected rect gets the light.

The tight bounding sphere for a cone of slant range `r` and half-angle `a` is:

```
a ≥ 45°:  center = apex + r·cos(a)·dir,   radius = r·sin(a)
a < 45°:  center = apex + (r/(2cos a))·dir, radius = r/(2cos a)
```

(circumsphere through the apex and the cap rim; the 45° switchover is where `tan a = 1`).²
For a 15° mover that is radius `0.518r` centred `0.518r` down the beam axis, versus the
naive apex-centred `r/cos a = 1.035r`. **~4× less volume, hugging the beam axis instead of
the apex.**

> **Correction, from measurement.** The claim that this "alone is most of the 331" is wrong,
> and the sphere is not by itself a tighter *broad phase*: its axis-aligned bounding box is
> larger than the cone's own AABB in both the axis-aligned and the diagonal case (a 15° cone
> of axial length `L` has AABB `0.536L × 0.536L × L`; its circumsphere's AABB is
> `1.072L` cubed). The sphere earns its place as the input to a **per-cell** test, not as a
> replacement bound. Implemented and measured in Phase 1, the cone-vs-sphere test cut
> references 9.4× but moved mean lights/cluster only 1.8×, because the limiter is cell shape
> in Z rather than the cone bound. See Phase 1 for the sweep that establishes this.

**(b) `gpu.rs::haze_tiles` is a second, worse culler that claims the full screen.** The haze
pass does not use `clusters.rs` at all. It has its own 2D 16 px tile grid built by
`haze_tiles`, which bounds the cone with `Vec3::splat(radius)` (a *cube* around the base
point, looser still), and — the actual smoking gun — on `behind_eye` it assigns the light to
`(0, 0, columns-1, rows-1)`:

```rust
let (x0, y0, x1, y1) = if behind_eye {
    (0, 0, columns - 1, rows - 1)
} else { /* ... */ };
```

Any cone whose bounding cube straddles the eye plane goes into **every tile on screen**.
With an orbit camera inside a rig of 512 fixtures, that is most of them, most of the time.
This is the "cones crossing the eye plane claim the full screen" failure, and it lives here,
not in `clusters.rs`.

The closed-form fix for screen bounds of a near-plane-straddling volume is Mara & McGuire,³
whose stated motivation is verbatim this problem: *"some common implementations … handled
the near clipping plane poorly."* They also name the case that has no answer — if the volume
encloses the eye (`c² < r²`) no silhouette exists and full-screen genuinely is correct. §3.1
makes that case disappear rather than detect it.

**(c) Two culling implementations for one concept.** `clusters.rs` (32 px tiles, 16 log-Z
slices, CSR headers+indices, `ClusterCache`) serves `scene.wgsl`; `haze_tiles` (16 px tiles,
2D only, `HazeTileCache`) serves `haze.wgsl`. Different tile sizes, different bounds math,
different cache keys, different failure modes. Per CLAUDE.md's "one canonical way" this is a
bug to unify, not a pattern to extend — and the drifted bounds math is exactly how the
full-screen fallback survived in one of them and not the other.

**(d) Shadow passes have no budget.** `MAX_FIXTURE_SHADOWS = 128` layers of a 256²
`texture_depth_2d_array`, one full render pass per dirty layer, dirty iff the view-proj bits
or the caster hash changed. Fixtures move every frame, so every frame is a 128-pass frame.
There is no throttle, no priority, no per-light resolution. For scale, Unity HDRP's
`k_DefaultMaxShadowRequests = 128` is the *ceiling* for a AAA pipeline;⁴ Epic's guidance for
moving local lights with VSMs is *"budget one or two per frame."*⁵

**(e) 9-tap PCF is the wrong filter for a raymarch.** `scene.wgsl` does 3×3
`textureSampleCompareLevel` per light per fragment; `haze.wgsl` does a nearest `textureLoad`
per integration sample per light (already a concession to cost). PCF is fundamentally not
prefilterable — every lookup is N compares and nothing amortises.⁶

**(f) Per-frame buffer churn.** `Renderer::storage()` is `create_buffer_init` — a fresh
allocation per call per frame, for cluster headers, indices, light SoA, shadow matrices, and
per-shadow globals. (A teammate is landing persistent buffers; treated as baseline here.)

---

## 2. Technique survey

### 2.1 Froxel volumetrics, and why it is not the beam path

**Wronski, SIGGRAPH 2014** (Assassin's Creed IV).⁷ Frustum-aligned 3D grid, **160×90×64 or
160×90×128, RGBA16F**, XY in NDC, exponential Z. Two volumes: (in-scattering RGB modulated
by density, scattering coefficient A) and (accumulated in-scatter RGB, transmittance A).
Passes: shadowmap downsample to a 1024×256 R32F ESM atlas + separable 11-px blur → CS
density+lighting → CS serial march along Z → PS apply with quadrilinear filtering.
**Xbox One total 1.1 ms** (lighting 0.43, scattering solve 0.116, apply 0.247). A parallel
prefix-sum for the Z integration measured **20–30 % slower** than the brute-force serial
march. Temporal: 1-sample jitter + reprojection, with the observation that in a
frustum-aligned volume disocclusion is a non-issue so history rejection reduces to
frustum-bounds rejection.

His own verdict on the output, slide 27: *"the produced effect is very soft and is missing
high frequency geometric details, but it fits our art direction."* Ours is the opposite art
direction.

**Hillaire, SIGGRAPH 2015** (Frostbite).⁸ 8×8 px froxels × 64 slices, aligned to the 16×16
tiled-deferred light tiles so cull lists are reused. Physically-based media (σ_a, σ_s,
emissive, g; σ_t = σ_s + σ_a, albedo ρ = σ_s/σ_t). The contribution worth stealing outright
is the **energy-conserving in-slice integration** — treating S and σ_t as constant over a
slice of length D:

```
∫₀ᴰ e^(−σ_t x) S dx  =  (S − S·e^(−σ_t D)) / σ_t

Sint = (S - S * exp(-sigma_t * D)) / max(sigma_t, 1e-4);
L += T * Sint;
T *= exp(-sigma_t * D);
```

This replaces Wronski's naive `AccumulateScattering`, which leaks light visibly once
volumetric shadows are on. PS4 @ 900p, 8×8: material voxelization 0.45 + light scattering
2.00 + accumulation 0.40 ms; local lights +1.1, sun +0.5, temporal +0.4. Temporal is a
**5 % EMA** on Halton-jittered samples, with material and scattering jitter kept in sync.
Volumetric shadow maps at 32³: **0.04 ms per spot**, 0.14 ms per point.

Read the scaling honestly: *light scattering* is the term that grows with light count and it
is 2 ms on PS4 with 14 point lights. And 512 × 0.04 ms of volumetric shadow map alone is
20 ms of PS4-era GPU.

**Unreal.**⁹ `r.VolumetricFog.GridPixelSize`, `GridSizeZ`, `DepthDistributionScale`.
PS4 High 1 ms, GTX 970 Epic 3 ms. Documented: shadow-casting point/spot lights are
*"approximately three times more expensive"*; **IES profiles are unsupported on volumetric
fog** — which for a moving-head product means gobos would not work; and the temporal
reprojection filter's documented cost is that *"fast-changing lights leave lighting trails."*
UE's separate light-shaft path¹⁰ is screen-space and radial-from-a-point (0.5–0.68 ms per
light on a GTX 680), so it cannot represent 512 arbitrarily oriented cones and collapses when
the source leaves frustum. Ruled out.

**Unity HDRP.**¹¹ 240×135×64 at 1080p default (8×8 px froxels). Denoising modes are
None/Reprojection/Gaussian/Both, with the docs stating plainly that Reprojection is
*"effective for static lighting but prone to ghosting"* and Gaussian is *"better for dynamic
lighting."* Local volumetric fog has **no volumetric shadowing** and visible aliasing at
volume boundaries.

**Naughty Dog, TLOU2, SIGGRAPH 2020** — the most on-point shipped data point.¹² Their §3 is
titled "Resolution and Sharpness":

> *"Since a 3D froxel grid is used, some sacrifices had to be made in the resolution… **it
> was harder to get sharp god ray effects.** We used 64 depth slices and supported multiple
> resolutions for the fog grid in screen space, ranging from 10×10 pixels per froxel to 4×4
> — 1.3 mil to 8.3 mil froxels."*

Two further findings that decide our design:

- *"Runtime lights [flashlights] could not use temporal compositing **because they move all
  the time** — the final 3D image of runtime lights had to be produced within one frame."*
  That is our fixtures, verbatim.
- *"Another family of artifacts was **patterning of fog lit by narrow god rays**; depending
  on how the grid aliased with light sources, different patterns would emerge **resembling
  Moiré patterns**. Special sampling and temporal jitter had to be implemented."*

**Do the froxel arithmetic for our case.** At 160×90×64 over a 60° vertical FOV, one froxel
subtends ≈0.56° horizontally. A moving head at the narrow end is 7–15° full angle and its
*edge* transition should be well under 1°. That is ~12–25 froxels across the beam and
**1–2 froxels across the penumbra** — the edge is reconstructed by trilinear interpolation
across two cells. And a mover slewing at 200 °/s crosses a froxel in **2.8 ms**, under one
frame, so a 5 % EMA (≈333 ms time constant at 60 Hz) smears it across ~60 frames of angular
travel. Frostbite's blend factor is categorically wrong for pan/tilt fixtures.

**Conclusion: froxels are correct for the ambient haze bed and wrong for the beams.**

### 2.2 Analytic beams — the path we are on, with names

**Sun et al., SIGGRAPH 2005**¹³ give the closed-form airlight integral for a point source in
a homogeneous medium. The whole integral factorises into analytic terms plus one 2D special
function independent of the physical parameters:

```
F(u,v) = ∫₀ᵛ exp(−u·tan ξ) dξ
L_a = A₀(T_sv, β, γ) · [ F(A₁, ξ_max) − F(A₁, ξ_min) ]
```

Their own error analysis: a **64×64 table of F is < 2 % max error** with bilinear
interpolation — a 16 KB R32F texture — and RMS error versus a multiple-scattering Monte
Carlo reference is **< 4 % for optically thin media**, which is exactly a hazed venue.
**Pegoraro et al., EGSR 2010**¹⁴ extend the closed form to general anisotropic phase
functions, i.e. Henyey–Greenstein `g` analytically rather than by sampling.

**Kulla & Fajardo, EGSR 2012**¹⁵ is what `haze.wgsl` already implements. PDF and inverse CDF:

```
pdf(t) = D / ((θ_b − θ_a)(D² + t²))
t_i    = D · tan((1−ξ_i)θ_a + ξ_i θ_b)
```

with heterogeneous media handled by decoupling: one march builds a per-segment transmittance
table `T_i = T_{i−1}·exp(−Δ_{i−1}σ_{t,i−1})`, after which any sample is O(1).

**Proxy-geometry beams** are what concert visualisers and NVIDIA's Volumetric Lighting SDK
(shipped in Fallout 4)¹⁶ actually do: rasterize a cone mesh per light, evaluate the airlight
integral in the fragment shader between the ray's entry and exit of that cone. Unity's
Volumetric Light Beam asset exposes cone **Segments** (mesh tessellation) as its silhouette
quality knob — i.e. **edge sharpness decoupled from volume resolution**, which is the whole
property we need. Known failure modes to design around: camera inside the cone (render
backfaces, clamp t_near); beams hitting geometry (clamp t_far to scene depth); no volumetric
shadowing from the analytic form alone; and a cone edge that is *too* hard without a small
angular smoothstep.

**Epipolar sampling** (Yusov, GDC 2013)¹⁷ is ruled out on Wronski's argument: *"No varying
media density; no multiple light sources… every different light source requires a different
epipolar sampling scheme."* It is a one-directional-light technique.

### 2.3 Clustered light culling

**Olsson et al., HPG 2012**¹⁸ established exponential Z partitioning, chosen so clusters stay
near-cubical (which is what makes sphere-vs-cluster tests well-conditioned):
`near_k = near·(1 + 2·tan θ / S_y)^k`. The number worth internalising is their teaser scene
at ~2400 lights: clustered **17 ms** (2.3 clustering + 1.5 assignment + 5.6 shading) versus
tiled **26 ms** (1.0 assignment + 17.7 shading). *Assignment got more expensive and the frame
got 35 % faster.* **We are not optimising culling cost; we are optimising shading cost** —
which is precisely the trade we forfeit by caching a stale, over-conservative structure.

**Drobot, SIGGRAPH 2017** (Call of Duty: Infinite Warfare)¹⁹ replaces the 3D grid with
**screen tiles + a 1D Z-bin array**, taking the good column from each:

| | depth discontinuities | XY resolution | memory vs Z |
|---|---|---|---|
| tiles | − | + | + |
| clusters | + | − | − |
| **tiles + Z-bins** | **+** | **+** | **+** |

Structures: a per-tile **flat bit array** (256 bits = 8 u32 shipped, 240×135 tiles at 1080p,
1,036 KB) — a fixed bitmask, not a variable-length index list, because a bitmask is what can
be scalarised per entity and has no per-cluster count cliff. Plus a **1D Z-bin LUT**,
uniformly spaced over the view-depth range, `ZBIN[z] = MIN_LIGHT_ID | MAX_LIGHT_ID`, 4 bytes
per bin, **8096 bins = 32 KB**. The trick is that lights are globally sorted by view-space Z,
so "lights overlapping this depth slab" is a contiguous index range that fits in 4 bytes
regardless of population. Measured: Hangar Fire opaque 9.00 → **7.65 ms** (15 %); Zombies
opening 5.7 → **4.6 ms** with scalarisation (80 %).

His rasterisation-based cull is also relevant to us: build the tile bitmask by rasterizing
light proxy meshes and `InterlockedOr`-ing the bit — **0.10 ms** for 3 full-screen lights at
240×135 with 4×MSAA + wave compaction; **0.32 ms** for 256 lights in a 60×40×32 cluster grid.
And the eye-inside handling: camera inside the light mesh → `Z Mode = GREATER` + backfaces;
outside → `LESS_EQUAL` + frontfaces.

**Unity HDRP fine pruning** (Mikkelsen, GPU Pro 7)²⁰ adds a 64×64 big-tile prepass (pure 2D
AABB overlap, no depth — 16× candidate reduction, zero correctness risk) and then, at 16×16,
an **exact per-pixel membership test against the tile's real depth samples**: each of 64
threads walks actual depth-buffer pixels, reconstructs view-space positions, and tests
`distSq ≤ radiusSq` / cone dot products / box bounds per coarse-list light, OR-ing results
into `ldsDoesLightIntersect`. That answers the question a bounding-volume test cannot: *does
this light touch any surface that actually exists here.* Note HDRP's clustered path packs
offset+count into one u32 with a 5-bit count — **a hard 31-lights-per-cluster cliff**; a
bitmask has no such cliff. HDRP is itself migrating to z-binning.²¹

**Cone culling math.** Wronski's cone-vs-sphere²² inverts the test — bound the *cluster* with
a sphere and test it against the cone analytically, because plane tests are the wrong shape
for a cone (*"the wide portion will often intersect the planes of subfrusta that are actually
just outside the narrower tip"*):

```glsl
float sphereRadius = length(aabb.extents);
vec3  v = aabb.center - spot.position;
float lenSq = dot(v,v), v1Len = dot(v, spot.direction);
float closest = cos(spot.angle)*sqrt(lenSq - v1Len*v1Len) - v1Len*sin(spot.angle);
bool  cull = (closest > sphereRadius) || (v1Len > sphereRadius + spot.range)
                                      || (v1Len < -sphereRadius);
```

Caveat that matters: this works best with square-ish cluster AABBs; long thin cells have
loose bounding spheres. Another argument for exponential Z (near-cubical cells), or for
Z-binning where the depth test is exact by construction.

**GPU vs CPU.** Bevy's move to GPU clustering measured **~20×** on its `many_lights`
benchmark.²³ Drobot's cluster cull is 0.32 ms on a 2013 PS4. Binning is 85 % of CPU
preprocessing cost for clustered shading.²⁴ And the GPU has information the CPU does not —
the depth buffer. We are caching an operation cheaper than validating the cache, and our
cache key is unsound anyway: the structure depends on light positions and scene depth, not
only the camera, so *any* camera-derived key (quantised or exact) is a correctness bug
waiting for a static camera with moving lights.

### 2.4 Shadows

**Atlas with per-light budgets.** DOOM (2016): one **8k×8k** atlas, per-light tile size
varying with distance, tiles explicitly *not* pinned across frames.¹ DOOM Eternal: 4096×8196,
24-bit, 3×3 PCF, allocation heuristic *"higher importance, larger screen area, closer to the
camera → larger portion of the atlas… evaluated dynamically"* — **three terms: artist
importance × screen area × 1/distance.**²⁵ Unity HDRP: five atlases, default 4096²,
`k_MinShadowMapResolution = 16`, `k_DefaultMaxShadowRequests = 128`, shelf packing with a
global uniform downscale on overflow; **HDRP has no per-frame throttle** — everything dirty
renders that frame.⁴ Godot's design is cheaper and better for us: a square atlas split into
**4 quadrants**, each independently subdivided into {1,4,16,64,256,1024} slots. O(1)
allocation, **tile identity stable across frames**, no defragmentation — HDRP needs a manual
`DefragAtlas()` precisely because it lacks this.²⁶

**Caching does not help us, and this is the important negative result.** Every published
static/dynamic split caches *static casters under a static light*. Our light frustums rotate
every frame, so the cached half is stale every frame. UE states it flatly: *"Any light
movement or rotation will invalidate all cached pages for that light."* We have the dual
problem — static geometry, moving lights — and its levers are per-light resolution, a
per-frame throttle, and light-independent caster representations. **Do not build the
static/dynamic split.**

**Throttle is the published answer.** The clearest formal statement is a patent (Warner
Bros., US 11,908,062 B2, *"Efficient Real-Time Shadow Rendering"*):²⁷ a generation queue,
priority sort dominated by distance, and *"if throttle value settings specify that at most a
determined number of shadows may be updated for a given frame… allows for an **upper bound on
how expensive the process will be**, at the cost of a slightly lower frame rate for shadow
updates."* Godot staggers PSSM splits 1/1, 1/2, 1/3, 1/4 and uses distance × radius × FOV for
omni/spot update rate.²⁸ UE's `DistantLightMode` categorises lights whose footprint fits one
128² page and updates them less often — a per-light update-rate tier keyed on screen
coverage, implementable in a plain atlas with no page tables.

Nobody publishes a movement epsilon; everyone is binary (moved ⟹ dirty). CryEngine's cascade
recentre test is the right *shape* — `(distanceBetweenCenters + measurements) > frustumSize/2`,
i.e. frustum-relative rather than absolute. **For a rotating moving head the analogue is
`Δθ > k·(fov / tileResolution)`** — invalidate when pan/tilt since the cached render exceeds
k texels of angular displacement. At a 256 px tile and a 15° beam that is ~0.06°/texel;
k = 2–4 hides under the filter kernel and buys real frames. (Synthesis, not a citation.)

**Scaling anchors.** Roblox "Future Is Bright": 73 MB VRAM of which 64 MB is shadow atlas;
worst case with no caching, a moving-lights-plus-moving-geometry scene costs **15 ms** of
shadow update — and **1000 non-shadow-casting lights cost 0.5 ms**.²⁹ Shadowing *is* the
cost. UE VSM on PS5: fully-invalidated local VSM 0.4–0.8 ms, cached 0.05 ms, guidance
*"budget one or two moving local lights per frame."*⁵ At 512 × 0.4 ms ≈ 200 ms that is a
250×-over verdict on our workload — and `r.Shadow.Virtual.Cache.StaticSeparate` is
**force-disabled on Metal**, stripping VSM of its main caching win on our target. VSMs are
out.

**Filtering.** PCF is not prefilterable: *"PCF requires sampling and comparing every
individual texel within the filter region."*⁶ Lauritzen's conclusion for a *constant* filter
width is that blurred filterable maps beat both PCF and SAT-VSM — which is exactly our
regime. **Moment Shadow Mapping** (Peters & Klein, I3D 2015)³⁰ stores four moments,
*"produc[ing] high quality results with a single shadow map sample per fragment using 64 bits
per shadow map texel"*, filterable with stock hardware bilinear/mip/aniso. The four-moment
choice was selected by automated evaluation of thousands of alternatives. EVSM needs 4× fp32
(128 bpp) for worse quality; SAVSM's summed-area table consumes ~18 of 23 mantissa bits at
512² and is a non-starter at 512 lights.

Critically, **Peters, Münstermann, Wetzstein & Klein, I3D 2016**³¹ explicitly validate MSM
for *single scattering* — because moment maps can be filtered directly, they combine with
prefiltered single scattering (Klehm et al., I3D 2014, which *"transforms the usually-employed
ray-marching into an efficient ray-independent texture filtering process"*). MSM trades
8 texture fetches for ~25 ALU per lookup; at 512 lights on M3 Max we are texture-bound, so
that is the right direction. And the beam is a low-frequency integrator, so MSM's
characteristic residual leak is far less visible integrated along a ray than at a hard
surface contact.

Secondary but real: MSM is an `rgba16unorm` **color** texture read with an ordinary filtering
sampler. No `sampler_comparison`, no `texture_depth_*`, no fragment-stage restriction — which
sidesteps naga's most fragile cross-backend corner.³²

**Atlas gutters.** Microsoft's shadow guidance³³ requires padding the outer rim of each
partition by half the PCF kernel, because filter taps index outside it. **In a 512-tile atlas
this is mandatory, not optional**: a 3×3 tap at a tile edge reads a *neighbouring light's*
depth. HDRP gets this free from 64-texel slot quantisation. A `texture_2d_array` avoids the
problem entirely — which is one of several reasons §3.3 chooses arrays over a packed atlas.

### 2.5 wgpu 26.0.1 constraints (verified against the pinned version)

| capability | status on Metal |
|---|---|
| `TEXTURE_BINDING_ARRAY` | ✅ (MSL 2.0+, macOS 10.13+) |
| `PARTIALLY_BOUND_BINDING_ARRAY` | ❌ Vulkan/DX12 only — every array slot must be populated |
| `BUFFER_BINDING_ARRAY` | ❌ Vulkan only |
| storage-texture binding arrays | ❌ lands in wgpu 28 |
| `max_texture_array_layers` | **2048** on Metal (wgpu default 256) |
| `max_texture_dimension_2d` | 16384 (Apple3+) |
| `textureSampleCompareLevel` in compute | ✅ (`SampleLevel::Zero` allows all stages in naga 26.0.1) |
| `textureSampleCompare` in compute | ❌ fragment only |
| `dispatchWorkgroupsIndirect`, `drawIndirect` | ✅ core WebGPU, no feature flag |
| `MULTI_DRAW_INDIRECT` | emulated (CPU loop) on Metal; removed as a feature in wgpu 27 |
| `MULTI_DRAW_INDIRECT_COUNT` | ❌ not on Metal |

**Consequences.** 512 shadow maps fit comfortably in a `texture_2d_array` (512 ≤ 2048) with
one binding, one sampler, uniform indexing, no feature flags, and free per-layer mip chains —
the boring correct answer, and better than a packed atlas because atlas mips are not free and
atlas gutters are. Compute-driven **dispatch** is available and is where the win is;
compute-driven **draw counts** are not, so do not architect around GPU-generated draw counts.
Prefer `textureSampleCompareLevel` if any comparison sampling survives into compute — but
MSM removes the question.

---

## 3. Chosen architecture

Three subsystems, each with one job, one owner, and a hard interface.

```
              ┌─────────────────────────────────────────────────┐
  fixtures ──►│ LightIndex (compute)                            │
              │  tiles 8px × 512-bit masks  +  1D Z-bin LUT      │
              └──────────────┬──────────────────────────────────┘
                             │  consumed ONLY by surface shading
                             ▼
  meshes  ──► opaque raster (scene.wgsl, MSAA 4×) ──► depth + color
                             │
              ┌──────────────┴──────────────┐
              ▼                             ▼
   ┌────────────────────────┐   ┌──────────────────────────────┐
   │ BeamPass (raster)      │   │ HazeVolume (compute)         │
   │  cone hull per fixture │   │  160×90×64 froxels, RGBA16F  │
   │  analytic ∫ + MIS      │   │  ambient/house bed only      │
   │  additive, half-res    │   │  5% EMA + Halton jitter      │
   │  NO temporal history   │   │  owns σ_t for the frame      │
   └───────────┬────────────┘   └──────────────┬───────────────┘
               └────────► composite ◄──────────┘
```

### 3.0 The partition invariant

A fixture is **either** an analytic beam **or** a froxel light — never both, and the type
system should say so, not a bool on a shared struct. The medium (σ_s, σ_t, g) is one physical
quantity shared by both; the *light set* is what is partitioned. Composite:

```
final = surface·T_haze + L_haze + Σ_beams L_beam · T_haze(beam exit)
```

The beam's internal attenuation is `exp(−σ_t·s)` along its own span; the haze volume supplies
camera-to-beam transmittance. Each light in-scatters exactly once; extinction has one owner.
This is "define errors out of existence" applied to double-counting.

For heterogeneous haze, sample σ from the froxel volume at the beam segment midpoint
(piecewise-homogeneous, per Kulla & Fajardo's decoupling). Simpler and probably sufficient:
keep the analytic path on a homogeneous global haze term and put all heterogeneity in the
froxel volume — which is what a venue hazer actually looks like.

### 3.1 BeamPass — cone proxy rasterization

**Replace the full-screen haze fragment pass with an instanced cone-hull draw.** One closed
cone mesh (apex + N-gon cap, N ≈ 24, one shared vertex buffer), instanced 512×, per-instance
data being the existing `LightCore`/`LightRest` SoA. The fragment shader keeps
`haze.wgsl`'s integrand **verbatim** — same ray∩cone∩sphere span, same equiangular+uniform
MIS, same gobo, phase, taper, noise, HDR policy. Additive blend (commutative, no sort).

What changes is the cost model: **O(Σ beam screen area)** instead of **O(screen area ×
average tile-list length)**. Empty sky costs nothing rather than a tile-list walk. This is the
NVIDIA VolumetricLighting / Unity VLB / Drobot proxy-raster architecture.

Four consequences worth stating explicitly:

- **The culling problem for beams disappears.** The rasterizer *is* the cull, and it is exact.
  There is no conservative bound to get wrong, no near-plane straddle to detect, no
  full-screen fallback. §1.3(b) becomes unrepresentable. `haze_tiles` and `HazeTileCache` are
  deleted, not fixed.
- **Camera inside the cone** is Drobot's eye-inside state flip: front-face + `LessEqual`
  normally; back-face + `Greater` when the eye is inside the hull. Determined per instance on
  the CPU (one dot product) and encoded as two draw batches, since Metal has no per-instance
  pipeline state.
- **Sharpness is geometry, not resolution.** The silhouette is a rasterized triangle edge;
  the interior falloff is the existing analytic `angular_profile` smoothstep. Neither depends
  on a volume resolution, which is the entire reason this beats froxels at our art direction.
- **Half-res with a full-res edge.** Render the additive beam buffer at 0.5× (as today) and
  upsample with a 4-tap nearest-depth (min |Δz|) filter. Wronski's objection to 2D half-res
  volumetrics is real — a low-res 2D buffer must pick one depth per texel at a discontinuity
  — but it bites the *interior* integral, which is low-frequency. The high-frequency signal
  is the cone edge, and that comes from geometry at full res.

**No temporal history on beams.** Naughty Dog's finding is dispositive: runtime lights *"could
not use temporal compositing because they move all the time."* The existing subframe jitter
accumulation (subframes=2) stays as intra-frame supersampling; the cross-frame history buffer
does not apply to this pass.

**Sample count budget.** Keep `haze_steps` adaptive rather than fixed at 8: scale samples by
projected beam solid angle, so a distant mover in the back of frame gets 4 and a beam filling
a third of the screen gets 16. This is the direct lever on p95 and it is per-instance, not
per-pixel, so it costs nothing to compute.

**Fallback path if the analytic integral proves adequate without sampling:** Sun et al.'s
`F(u,v)` in a 64×64 R32F LUT (< 2 % error, 16 KB) collapses the whole per-pixel loop to a
handful of ALU plus two texture fetches, and Pegoraro et al. extend it to HG anisotropy.
Worth a spike in Phase 2b — but the MIS estimator already handles gobos, range taper and
noise, which the closed form does not, so this is an optimisation of the homogeneous
un-goboed case, not a replacement.

### 3.2 LightIndex — tiles + Z-bins, GPU compute, one implementation

Delete `clusters.rs`'s CSR grid and `gpu.rs::haze_tiles`. One module, one structure, one
consumer (surface shading in `scene.wgsl` — beams no longer need it).

**Chosen parameters, with rationale:**

| parameter | value | why |
|---|---|---|
| tile size | **8 px** → 240×135 tiles at 1080p | Drobot/Frostbite/HDRP/UE all converge here; 32 px today is coarse enough that a beam's tile footprint is mostly false positives |
| per-tile storage | **512-bit mask, 16 × u32** | fixed 2,072 KB regardless of occupancy; no count cliff (cf. HDRP's 5-bit, 31-light ceiling); scalarisable per entity, which a CSR index list is not |
| Z structure | **1D Z-bin LUT, 4096 bins uniform in view depth**, `min\|max` u16 | 16 KB; lights globally sorted by view Z make "lights in this slab" a contiguous range |
| build | **compute shader, `atomicOr`** | Bevy measured ~20× over CPU; Drobot 0.32 ms for 256 lights on a PS4; and the GPU has the depth buffer |
| bounds | **tight cone bounding sphere** (§1.3a) then **Wronski cone-vs-sphere** per tile | ~4× tighter than the AABB before any per-tile refinement |
| cache | **none** | rebuild every frame |

512 bits fits our hard fixture ceiling exactly. If that ceiling ever moves, the mask widens
to 32 words (1024 lights, 4.1 MB) — still fixed-size, still no cliff.

**Deleting the cache is a correctness fix, not just a perf change.** The structure depends on
light positions and scene depth; any camera-derived key — exact bits or quantised — is wrong
for a static camera with moving lights. Quantising the key (the tactical fix landing now)
makes the miss rate acceptable but leaves the unsoundness; Phase 4 removes the question.

**Skip for now:** the 64×64 big-tile prepass (add only if still candidate-bound after tight
bounds) and HDRP-style fine pruning against real depth samples (most expensive, needs the
depth prepass, last resort). Both are noted as escalation paths, not initial scope.

### 3.3 Shadows — tiered arrays, hard throttle, moment maps

**Storage: three `texture_2d_array`s, one per resolution tier, `rgba16unorm`.**

| tier | resolution | layers | memory | assignment |
|---|---|---|---|---|
| A | 512² | 32 | 64 MB | top 32 by importance × screen coverage × 1/distance |
| B | 256² | 128 | 64 MB | next 128 |
| C | 128² | 352 | 44 MB | remainder |

**172 MB total**, in the same order as UE's 150 MB cached-shadow budget and above Roblox's
shipping 64 MB. Tier assignment uses DOOM Eternal's three terms verbatim (artist importance ×
screen area × 1/distance), recomputed per frame with hysteresis so fixtures do not oscillate
between tiers.

Arrays rather than a packed atlas because: per-layer mip chains are free (and MSM wants
prefiltering), no gutter/bleed problem at tile edges, uniform indexing, one binding, and
512 ≤ `max_texture_array_layers` = 2048 on Metal. Fixed tiers rather than HDRP's
sort-and-shelf because tier slots give O(1) allocation and **stable slot identity across
frames**, which is what any caching or reprojection needs and what forces HDRP to ship a
manual `DefragAtlas()`.

**Representation: 4-moment MSM**, replacing the 9-tap PCF in `scene.wgsl` and the nearest
`textureLoad` in `haze.wgsl`. One bilinear tap per lookup, prefilterable, 64 bpp, no
comparison sampler. The prefilter property is worth more inside a beam integral than anywhere
else: blur the moment map once per light per frame and *every* sample of *every* ray reads the
already-softened result. PCF fundamentally cannot amortise that.

**Update policy — the direct fix for the 12–28× tail:**

1. Hard per-frame budget **N = 16 shadow renders**, tuned against the profile, not against
   the light count. This bounds worst-case cost *unconditionally*, which is the entire point.
2. Priority queue scored on `screen_coverage × intensity × angular_delta × age`.
3. Angular invalidation threshold `Δθ > k·(fov/tileRes)`, k = 2. Slow-moving fixtures skip
   entirely rather than burning budget.
4. Tier C fixtures (footprint under ~one tile) get a fixed low update rate — UE's
   `DistantLightMode` idea without the page tables.

N = 16 over 512 shadowed fixtures is 32 frames of worst-case refresh latency, which sounds
alarming and is not: the angular threshold means only fixtures actually slewing request
updates, and the ones that are slewing fastest are precisely the ones whose stale shadow is
least legible. If it does read as lag, raise N against measured headroom — that is the knob,
and it is a knob because the budget exists.

**Caster set.** Killzone Shadow Fall's lesson is orthogonal to everything above and probably
worth more than any of it: shadow rendering was up to **60 % of their lighting budget**, 5000+
draw calls, ~3M triangles, fixed with offline-generated low-poly shadow proxies — **60–80 %
triangle reduction, one draw call per light**.³⁴ At 512 lights, draw-call count is a
first-order term. The venue geometry (truss, deck, risers) is static and small; a
precomputed proxy caster set submitted as one instanced draw per shadow render should be part
of Phase 3, not a later optimisation.

### 3.4 HazeVolume — froxels for the ambient bed only

Today the ambient medium fill is 8 uniform stratified taps per pixel per frame (`amb_end =
min(hit_dist, 24.0)`), which is a per-pixel march for a low-frequency, view-independent-ish
signal. That is exactly what froxels are for.

- **160×90×64 RGBA16F**, frustum-aligned, exponential Z (Wronski's shipped configuration;
  12 px froxels at 1080p, which is coarse and correct for this signal).
- Two volumes: (in-scatter RGB, σ_t A) and (accumulated in-scatter RGB, transmittance A).
- **Hillaire's energy-conserving in-slice integration**, not Wronski's naive accumulate.
- **Frostbite temporal: 5 % EMA, Halton jitter, material and scattering jitter in sync,
  frustum-bounds history rejection.** This is where a 333 ms time constant is *correct*,
  because the signal is house light, ambient SH and drifting hazer turbulence, none of which
  slews at 200 °/s.
- Add **neighbourhood clamping** (3×3×3 current-frame min/max around the reprojected sample)
  before the EMA. Not in the 2014/2015 decks — both predate its wide adoption — but it
  composes directly and is the standard defence against the ghosting HDRP's own docs warn
  about.
- The volume **owns σ_t for the frame** and supplies camera-to-beam transmittance to the beam
  pass (§3.0).
- Free bonus, per Naughty Dog: the lit fog grid can light particles/fog cards later at
  near-zero marginal cost.

This is the *last* phase, and it is explicitly optional. If the ambient bed reads fine as
8 taps, the froxel volume buys frame-time headroom and a place to put local fog volumes
later, not image quality.

### 3.5 Frame architecture and budget discipline

- **Compute:** LightIndex build, HazeVolume injection + Z-integration, shadow moment
  prefilter blur. **Raster:** opaque (MSAA 4×), shadow renders, beam cone hulls, composite.
- **Buffers:** one persistent growable arena per category (light SoA, tile masks, Z-bins,
  shadow matrices), written with `write_buffer`, grown by doubling, never reallocated per
  frame. `Renderer::storage()`'s `create_buffer_init` per call is the current pattern and
  should not survive.
- **Indirect:** use `dispatchWorkgroupsIndirect` to size the beam/froxel work from GPU-side
  counts. Do **not** design around GPU-determined draw counts — `MULTI_DRAW_INDIRECT_COUNT`
  is not available on Metal.
- **Every unbounded loop gets a budget.** Shadow renders: N per frame. Beam samples: scaled
  by projected solid angle. Froxel work: fixed grid, fixed cost by construction. The reason
  our max/p95 is 12–28× is that *nothing* in the current frame has a ceiling; the reason
  engines hold p99 is that everything does.
- **Profile gate:** `volumetric-profile-m3-max.json` should grow a `gpu_volumetric_max`
  budget alongside the p95 ones. A p95 budget cannot fail on a 140 ms frame, which is how a
  27× tail passed `all_within_budget: true`.

---

## 4. Migration plan

Phase 0 was the tactical work, and it landed folded into Phase 1: camera-quantised cluster
cache, persistent buffers, cascade and shadow bind-group caching, and the honest profiler.
Two items on its original list were dropped on inspection rather than built —
**shadow round-robin** (rejected, see Phase 3) and **per-segment haze shadow sampling**,
which assumed a 9-tap PCF in the haze inner loop. `haze.wgsl` already does a single nearest
`textureLoad`, and `angular <= 0.0` culls samples outside the cone *before* the shadow fetch,
so there was little cost there to reclaim and a real quality cost to reclaiming it.
Everything below is compatible with what landed and, where it supersedes it, says so.

### Phase 1 — Bounds correctness and the full-screen fallback — **LANDED, with one item moved to Phase 4**

The gate below was run. It **failed**, and the measurement moved the cone-vs-cluster test
into Phase 4. What landed:

1. Near-plane-correct bounds in **both** cullers, sharing one clipping primitive
   (`clusters::box_corners` / `for_each_clipped_vertex`). The `behind_eye ⇒ whole screen`
   branch is gone from `clusters.rs::bounds_for` *and* `gpu.rs::haze_tiles`. This is where
   the phase's GPU win came from: `gpu_total` p95 −22 % (transport-128) and −27 %
   (fixture-shadows-120) on a camera-orbit benchmark.
2. Persistent grow-only cluster storage buffers (`queue.write_buffer` into a retained
   allocation instead of `create_buffer_init` per rebuild) — removes ~23 MB/frame of
   allocate-and-upload at 512 cones.
3. Sun-cascade dirty checking, sharing `ShadowCacheKey` with the fixture maps, and
   per-fixture shadow bind groups built only for maps that will actually render.
4. A quantised cluster cache key (~16 % fewer rebuilds while orbiting; nothing in a live
   show, where `topology_hash` invalidates first — see §3.2).
5. An honest profiler: animated scene, `--orbit` (camera only) and show (camera + heads)
   modes, gating on `gpu_total.max` as well as p95, and `mean_lights_per_cluster`
   (`light_references / occupied_clusters`) as a first-class budgeted field.

#### The gate failed: cone-vs-sphere does not pay on a CPU CSR builder

The tight cone bounding sphere plus a per-cluster Wronski cone-vs-sphere test was built and
measured. It was *correct* — the conservativeness property (sampled cone interior points are
never absent from their cluster) held throughout — but:

| metric, transport-128 orbit | before | with narrow phase | |
|---|---|---|---|
| `light_references` | 1,412,926 | 150,358 | 9.4× better |
| **mean lights/cluster** | 86.6 | 49.0 | **1.8× — gate wanted ~10×** |
| `max_lights_per_cluster` | 128 | 128 | unchanged |
| `cpu_cluster` p95 | 2.94 ms | 6.88–7.17 ms | **2.4× worse** |

Two candidate explanations were ruled out by measurement:

- **Not scene shape.** A realistic venue rig (512 fixtures on a 20 m truss, each aimed at its
  own patch of a 16 m stage) culls no better than the profiler's packed layout: mean 121.6 vs
  114.4.
- **Not the far plane.** Fitting far to the room makes it *worse*: 200 m → 121.6,
  100 m → 138.5, 50 m → 138.7, 25 m → 166.1.

**It is Z resolution — the cells are splinters.** With 16 logarithmic slices over
0.1–200 m, a cluster at 20 m measures roughly **0.5 m × 0.5 m × 11.6 m**, some twenty times
longer than it is wide. Any bounding proxy for a cell that shape is enormous relative to the
cell, so no amount of tightening the *cone* bound lets the test reject much. Sweeping
`CLUSTER_DEPTH_SLICES` on the spread rig confirms it:

| slices | mean lights/cluster | max | `light_references` |
|---|---|---|---|
| 16 (current) | 121.6 | 404 | 263,579 |
| 64 | 72.5 | 321 | 303,407 |
| 256 | 55.0 | 255 | 574,507 |

Note the third column. More Z resolution improves the mean but *inflates* the CSR reference
count, because more occupied cells means more entries — which is exactly why §3.2 specifies
4096 Z-bins **with a 512-bit per-tile mask** rather than more slices in an index list. The
mask decouples Z resolution from reference count, and a compute build makes ~32 k cluster
tests free.

**Conclusion: the cone test is a Phase 4 item, not a Phase 1 one.** On the CPU CSR builder it
costs 2.4× the build time to miss its target; on Phase 4's structure it is nearly free and
lands against cells thin enough to reject. It was reverted rather than shipped.

**Gate (as run):** lights/cluster must drop by an order of magnitude. It dropped 1.5–1.9×.

### The zoom question — measured, and it is not fragment-bound

A reported "freezes whenever I zoom in" was investigated as coverage-bound
volumetric cost. It is not, on three independent grounds:

- **Half-res volumetrics already ship.** `LIVE_HAZE_RESOLUTION = 0.5`, clamped
  0.25–1.0, with the composite's depth-guided bilateral upsample.
- **Coverage does not drive cost.** With beam coverage verified by the
  profiler's `--capture` mode (lit-pixel fraction per case), 24 % vs 63 %
  coverage measured 0.94/0.96/0.95 ms against 0.83/0.85/0.78 ms across three
  alternating runs — *cheaper* zoomed in, because lights-per-tile falls as cones
  leave the frustum and cancels the coverage rise.
- **The product cannot reach the expensive camera anyway.**
  `Framing::NEAR_MARGIN = 1.25` clamps the dolly to 1.25 × the rig's radius, so
  the camera never enters the beam volume.

What zooming *does* do is worsen the tail of an already over-budget frame — at
2057 draws with unculled shadows, zoomed p95/max were 11.22/19.19 ms against
8.66/10.88 wide. So zoom was the trigger, not the cause, and caster culling
(§Phase 3) takes those to 2.97/3.91 ms.

The lesson for this file: **a benchmark whose scene is not representative
measures the wrong term forever.** Two of the profiler's axes — camera distance
and geometry density — did not exist, and between them they hid the renderer's
largest cost behind its smallest.

### Phase 2 — BeamPass as cone proxy geometry
*Supersedes `haze_tiles` and `HazeTileCache` entirely.*

1. Cone hull mesh + instanced additive draw; port the `fs_main` integrand unchanged.
2. Eye-inside/outside batching with the depth-state flip.
3. Per-instance adaptive sample count from projected solid angle.
4. Depth-aware 4-tap upsample of the half-res beam buffer; verify against
   `goldens/contracts/*` (the analytic integrand is unchanged, so contract goldens should
   move only by resampling, not by radiance).
5. Delete `haze_tiles`, `haze_tile_key`, `HazeTileCache`, `HAZE_TILE_SIZE`.

**Expected:** cost proportional to beam screen coverage. p50 at 512 cones roughly halves
(most of the screen is not beam); **p95 and max collapse together**, because the pathological
case — a tile list containing every light — no longer exists. Target: max/p95 under 3×.

*2b (optional spike):* Sun et al. `F(u,v)` LUT for the homogeneous un-goboed case, measured
against the MIS estimator for both cost and contract-golden delta.

### Phase 3 — Shadow tiers, throttle, moment maps
*Partly landed: the count cap below is in, the tiers and moment maps are not.*

**Landed ahead of this phase, because the freeze investigation needed it.**
`MAX_FIXTURE_SHADOWS` is 16, not 128, and slots are assigned per frame by
priority — apparent size from the eye scaled by intensity — with hysteresis, so
a resident only loses its slot to a challenger 1.25x better and two near-equal
cones cannot trade it every frame and flicker. Cones without a slot carry a
negative `shadow_slot` and cast no shadow; `scene.wgsl` and `haze.wgsl` index
the atlas by slot rather than by light index.

**Also landed: per-cone caster culling.** Every fixture shadow map used to
redraw *all* opaque geometry (`gpu.rs`, `draw_range(&mut pass, 0..opaque,
false)`) — no culling of any kind. Each map now draws only the casters whose
world bounding sphere the cone actually reaches (Wronski cone-vs-sphere against
a per-mesh local sphere computed once with the geometry upload). It is
output-neutral by construction: a caster the cone does not reach contributes
nothing to that map. An empty caster list still runs its pass, because the
attachment's `LoadOp::Clear` is what makes the map read as unoccluded and
skipping would leave the slot's previous tenant's depth behind.

**This was the dominant cost in the whole renderer and the profile scene hid
it.** The synthetic scene has 17 opaque draws; a real rig draws a body per
fixture. Adding a geometry-density axis (`geometry_copies`) to reach 2057 draws
showed the shadow passes submitting 16 × every opaque draw ≈ 33,000 draws per
frame:

| 2057 draws, 120 cones | `gpu_volumetric` p50 | `gpu_total` p50 | p95 | max | `cpu_encode` p95 |
|---|---|---|---|---|---|
| shadows off | 0.02 ms | 0.87 ms | 1.42 | — | 7.4 ms |
| shadows on, no cull | 0.02 ms | 9.99 ms | 12.28 | 14.90 | 23.7 ms |
| shadows on, **culled** | 0.02 ms | **2.53 ms** | **3.41** | **6.09** | **10.5 ms** |

Note the volumetric column: **0.02 ms**. At realistic geometry density the
volumetric pass is not a term worth optimising — the beams are occluded by the
geometry in front of them, and the frame is shadow- and surface-bound. Sparse
scenes are unaffected by the cull (`fixture-shadows-120`, 17 draws, 1.10 ms
before and after): there is nothing to cull when there is nothing there.

This is strictly cheaper than the proxy meshes below and should come first —
proxies reduce per-draw cost, culling removes the draws.

This is a **count cap, not the refresh budget** originally planned here, and the
distinction is the point. Both spend the same per-frame budget. Refreshing N
stale maps in rotation keeps all 120 fixtures casting but leaves each shadow up
to `120/16 ≈ 8` frames behind its own beam — and a shadow that does not line up
with the beam casting it reads as broken, where a fixture that simply casts none
reads as unlit. The cap also cuts the per-fragment sampling cost, which
staleness does not. Measured at 120 moving heads: `gpu_total` p95 5.58 → 1.40 ms
and `cpu_encode_submit` p95 9.50 → 4.92 ms, with contract goldens unchanged
(every golden scene has fewer than 16 casters).

The tiered arrays, moment maps and caster proxies below are still outstanding.


1. Three tiered `texture_2d_array`s + per-frame tier assignment with hysteresis.
2. Priority queue + hard budget N = 16 + angular invalidation threshold. **This is the sole
   refresh policy — round-robin is explicitly rejected.** Refreshing K maps per frame in
   rotation lags a moving head's shadow by up to `maps / K` frames, and a moving head is
   precisely the case that dirtied the map: round-robin trades a correct shadow for a
   visibly wrong one, worst exactly where it is most looked at. The priority queue spends the
   same budget on the maps that matter this frame and lets a static fixture keep a valid map
   indefinitely, which round-robin cannot express.
3. MSM: render moments instead of depth; prefilter blur in compute; replace the PCF in
   `scene.wgsl` and the `textureLoad` in `haze.wgsl` with a single bilinear moment tap.
4. Precomputed low-poly caster proxies, one instanced draw per shadow render.

**Expected:** this is the phase that fixes `fixture-shadows-*` max. Shadow raster becomes a
flat ~2–3 ms regardless of how many fixtures moved. Surface shading loses 8 of 9 shadow taps
per light per fragment; beam shadow sampling gains prefiltered soft edges it currently fakes
with temporal accumulation.

**Risk:** MSM light-bleeding on near-occluder/far-receiver pairs (a truss element in front of
a distant floor is exactly the bad case). Mitigation is the standard moment bias plus the
observation that a beam integral hides it far better than a surface contact does. Validate on
`occluded-beam` and `fixture-shadowed-beam` contract goldens before deleting the PCF path.

### Phase 4 — LightIndex rewrite (tiles + Z-bins in compute)

1. New module replacing `clusters.rs`: 8 px tiles, 512-bit masks, 4096 Z-bins, built in
   compute with `atomicOr`, no cache.
2. `scene.wgsl` iterates the mask via the Z-bin range (Drobot's word-range + edge-mask loop).
3. Delete `ClusterCache`, `ClusterCacheKey`, `topology_hash`, and the CSR headers/indices.
4. **Tight cone bounding sphere + Wronski cone-vs-sphere test, moved here from Phase 1.** The
   maths is §1.3(a); the reason it belongs here is the Phase 1 measurement. The test only
   rejects usefully once cells stop being Z-splinters, and only pays for itself once the
   build is a parallel compute dispatch instead of a serial CPU pass. Phase 1 measured it at
   9.4× fewer references for 2.4× the CPU — here both of those become wins.

**Expected:** CPU cluster build (13.8 ms cold, and the whole `cpu_encode_submit` contribution)
goes to zero; GPU cull lands around 0.2–0.4 ms by Drobot's PS4 numbers scaled to M3 Max.
Surface shading gets Drobot's measured 9–20 % from tighter Z. The correctness win — no cache
key that can be stale — matters more than either.

*Optional:* WGSL subgroup ops (`subgroupOr`, `subgroupBroadcastFirst`, `subgroupMin/Max`) for
scalarisation. Drobot measured 5.7 → 5.1 ms standalone. Calibrate expectations against the
non-scalarised row (91 %), not the scalarised one (80 %), until portability is verified.

### Phase 5 — HazeVolume froxel grid (optional)

160×90×64 RGBA16F, Hillaire integration, Frostbite temporal + neighbourhood clamping,
replacing the 8-tap ambient march. Buys headroom and a home for local fog volumes; does not
change beam quality.

### The presentation seam (vendor patch)

Not a renderer phase, but it lives next to one and the numbers are large enough
to record. Every presented frame used to travel: readback `Vec<u8>` → `Arc<[u8]>`
(copy) → `.to_vec()` (copy) → a **new** `RenderImage`, whose `ImageId` comes from
a global counter, so the sprite atlas saw a key it had never seen, allocated a
viewport-sized `MTLTexture` for it, uploaded, and then destroyed the previous
one when `drop_image` left it unreferenced. At 2560x1440 that is ~15 MB
allocated, ~30 MB copied and ~15 MB destroyed, per frame, on the UI thread.

Now: the pixel buffer is owned and moved end to end, every stage frame is
published under one process-wide `ImageId`, and `gpui` grew
`PlatformAtlas::update` — refresh a resident tile in place, with a default
implementation that drops the tile so the next insert rebuilds it (correct on
every backend, cheap only where implemented, currently Metal). A traced run
shows one insert and 240 in-place refreshes over 240 frames, with the only other
miss a genuine window resize.

Measured on the UI thread at 2560x1440, playing, 120 fixtures:

| | before | after |
|---|---|---|
| median | 12.3 ms | **7.2 ms** |
| p95 | 20.5 ms | **8.4 ms** |
| max | 38.3 ms | **~10 ms** |

The remaining cost is the `replace_region` upload itself, which only a zero-copy
`IOSurface` handoff removes — the seam `viewport.rs`'s module doc anticipates,
and the natural next step if this ever needs to get cheaper.

That step has since been taken: the frame is now written once, into an
`IOSurface` both devices address, and neither crossing survives. See
[`presentation-seam.md`](presentation-seam.md) for the design and the numbers.

### Worker death is loud and recoverable

A panic on the renderer thread used to be *silent*: the thread disappeared,
`take_latest` returned `None` for ever, and the stage went on painting its last
good frame while the UI stayed responsive. That presents as "it froze and won't
move" with nothing anywhere saying why, and it is reachable by more than
ordinary bugs — a wgpu validation error, a lost device or a driver fault all
arrive as a panic on that thread, which is why auditing `unwrap`s cannot close
this class on its own.

`supervised_worker` (`viewport.rs`) now wraps the loop in `catch_unwind` and, on
a panic: prints the reason to stderr (deliberately not a logging facade — this
line has to reach a terminal someone is looking at), hands the in-flight slots
back as an error so the caller stops waiting on a frame nothing is drawing, and
re-enters `render_worker`, which acquires a fresh device. Bounded at three
restarts so a renderer that dies immediately cannot spin rebuilding devices.

The app drops its last presented frame alongside the error, so a stopped
renderer cannot leave a stale picture that looks like a live one. Note the
resulting state is *sticky*: the stage shows the failure until the pane is
reopened, rather than healing itself. That is deliberate for now — a silent
recovery would hide a real fault — but it means "restarting" in the message
promises less than it sounds like, and a self-healing stage is worth doing
properly rather than by accident.

### Re-baselining the profile golden

`goldens/volumetric-profile-m3-max.json` is a measurement artifact, not a gate — nothing
reads it programmatically. Capture it with:

```
cargo run -p luma-render --release --bin profile-volumetrics \
  > gpui/crates/render/goldens/volumetric-profile-m3-max.json     # show case
cargo run -p luma-render --release --bin profile-volumetrics -- --orbit   # editor-drag case
```

**Capture on an idle machine, and check `uptime` first.** Timings taken while anything else
is compiling are worthless and, worse, poisonous: a contended baseline makes every later
comparison lie. For scale, one transport-512 run under load reported `gpu_volumetric` p95 at
30.1 ms; two immediate re-runs on the same binary gave 5.57 and 5.38. GPU p95 is the most
robust figure; `max`-of-600 and CPU-side numbers are the first to be corrupted by scheduler
noise. The golden currently checked in predates the animated benchmark and should be replaced
wholesale rather than diffed against.

Note that **every case reports `within_budget: false`** under the animated benchmark with
`gpu_total.max` gating — baseline included. The renderer has never held a 60 Hz worst-case
frame with a moving camera; the older `all_within_budget: true` was an artifact of a static
scene and p95-only gating, not a regression introduced since.

---

## 5. Open questions

- **Gobo projection through shadows.** UE does not support IES on volumetric fog at all. Our
  gobo is evaluated analytically per sample, which is better — but interaction between gobo
  transmission and the MSM prefilter blur is unexplored. Likely fine (they multiply), worth a
  golden.
- **MSAA and the beam pass.** Beams currently composite after a 4×MSAA resolve. Rasterizing
  cone hulls at half-res against a resolved depth buffer needs a decision on whether the cone
  silhouette wants its own coverage AA or whether the analytic angular smoothstep is enough.
  Suspect the latter — the falloff is already a smoothstep in angle, not a hard cutoff.
- **512 as a real ceiling.** The 512-bit tile mask and the tier table both hard-code it. If
  the product target is "a stadium rig", state the real number now; widening the mask later
  is cheap but the tier memory table is not.
- **`cluster_stats.max_lights_per_cluster` equals the cone count in every profile case**
  (32/128/512/120). That is a tautological metric — it will read "all lights" until the fill
  is fixed, so it cannot detect the regression it exists to detect. Replace with the
  average and the p99.

---

## References

1. Adrian Courrèges, *DOOM (2016) — Graphics Study*, 2016. https://www.adriancourreges.com/blog/2016/09/09/doom-2016-graphics-study/ — reporting Tiago Sousa & Jean Geffroy, *The Devil is in the Details: idTech 666*, SIGGRAPH 2016 Advances in Real-Time Rendering. https://advances.realtimerendering.com/s2016/Siggraph2016_idTech6.pdf
2. Simon Coenen, *Optimizing spotlight intersection in tiled/clustered light culling*. https://simoncoenen.com/blog/programming/graphics/SpotlightCulling (note: the published `>45°` branch conflates the axial offset with the sphere radius; the corrected slant-length form is given in §1.3a)
3. Michael Mara & Morgan McGuire, *2D Polyhedral Bounds of a Clipped, Perspective-Projected 3D Sphere*, JCGT 2(2):70–83, 2013. https://jcgt.org/published/0002/02/05/paper.pdf
4. Unity, *Shadows in HDRP*, and `HDShadowManager.cs` / `HDDynamicShadowAtlas.cs` / `HDCachedShadowManager.cs`. https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@17.0/manual/Shadows-in-HDRP.html
5. Epic Games, *Virtual Shadow Maps in Unreal Engine*. https://dev.epicgames.com/documentation/en-us/unreal-engine/virtual-shadow-maps-in-unreal-engine ; StraySpark, *VSM Optimization for Open Worlds in UE5.7*. https://www.strayspark.studio/blog/virtual-shadow-map-optimization-open-worlds-ue5-7
6. Andrew Lauritzen, *Summed-Area Variance Shadow Maps*, GPU Gems 3 ch. 8. https://developer.nvidia.com/gpugems/gpugems3/part-ii-light-and-shadows/chapter-8-summed-area-variance-shadow-maps
7. Bartłomiej Wroński, *Volumetric Fog: Unified Compute Shader Based Solution to Atmospheric Scattering*, SIGGRAPH 2014 Advances in Real-Time Rendering. https://bartwronski.com/wp-content/uploads/2014/08/bwronski_volumetric_fog_siggraph2014.pdf
8. Sébastien Hillaire, *Physically Based and Unified Volumetric Rendering in Frostbite*, SIGGRAPH 2015 Advances in Real-Time Rendering. https://advances.realtimerendering.com/s2015/ ; reference integration: https://www.shadertoy.com/view/XlBSRz
9. Epic Games, *Volumetric Fog in Unreal Engine*. https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-fog-in-unreal-engine
10. Epic Games, *Using Light Shafts in Unreal Engine*. https://dev.epicgames.com/documentation/en-us/unreal-engine/using-light-shafts-in-unreal-engine
11. Unity, *Fog* and *Local Volumetric Fog*, HDRP 14.0. https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@14.0/manual/Override-Fog.html
12. Artem Kovalovs, *Volumetric Effects of The Last of Us: Part Two*, SIGGRAPH 2020 Talks. https://history.siggraph.org/wp-content/uploads/2022/08/2020-Talks-Kovalovs_Volumetric-Effects-of-The-Last-of-Us-Part-Two.pdf
13. Bo Sun, Ravi Ramamoorthi, Srinivasa Narasimhan, Shree Nayar, *A Practical Analytic Single Scattering Model for Real Time Rendering*, SIGGRAPH 2005. http://www.cs.cmu.edu/~ILIM/publications/PDFs/SRNN-SIGGRAPH05.pdf
14. Vincent Pegoraro, Mathias Schott, Steven Parker, *A Closed-Form Solution to Single Scattering for General Phase Functions and Light Distributions*, EGSR 2010. https://www.sci.utah.edu/~vpegorar/research/2010_EGSR.pdf
15. Christopher Kulla & Marcos Fajardo, *Importance Sampling Techniques for Path Tracing in Participating Media*, EGSR 2012. https://diglib.eg.org/items/a3eae150-9f7d-4edd-ba74-520c0a20212b
16. Nathan Hoobler, *Fast, Flexible, Physically-Based Volumetric Light Scattering*, GDC 2016 (NVIDIA). https://gdcvault.com/play/1023519/Fast-Flexible-Physically-Based-Volumetric ; open reference: https://github.com/SlightlyMad/VolumetricLights
17. Egor Yusov, *Practical Implementation of Light Scattering Effects Using Epipolar Sampling and 1D Min/Max Binary Trees*, GDC 2013. https://gdcvault.com/play/1018227/Practical-Implementation-of-Light-Scattering
18. Ola Olsson, Markus Billeter, Ulf Assarsson, *Clustered Deferred and Forward Shading*, HPG 2012. https://www.cse.chalmers.se/~uffe/clustered_shading_preprint.pdf
19. Michal Drobot, *Improved Culling for Tiled and Clustered Rendering in Call of Duty: Infinite Warfare*, SIGGRAPH 2017 Advances. https://www.advances.realtimerendering.com/s2017/2017_Sig_Improved_Culling_final.pdf
20. Morten S. Mikkelsen, *Fine Pruned Tiled Light Lists*, GPU Pro 7, 2016; Unity HDRP LightLoop source. https://github.com/Unity-Technologies/FPSSample/tree/master/Packages/com.unity.render-pipelines.high-definition/Runtime/Lighting/LightLoop
21. Evgenii Golubev, *Binned Lighting Technique for the HDRP*, Unity-Technologies/Graphics PR #2629. https://github.com/Unity-Technologies/Graphics/pull/2629
22. Bart Wronski, *Cull that cone! Improved cone/spotlight visibility tests for tiled and clustered lighting*, 2017. https://bartwronski.com/2017/04/13/cull-that-cone/
23. Bevy 0.19 release notes (GPU clustering, ~20× on `many_lights`). https://bevy.org/news/bevy-0-19/
24. Yuriy O'Donnell & Matthäus Chajdas, *Tiled Light Trees*, I3D 2017. https://www.kayru.org/publications/TiledLightTrees-preprint.pdf
25. Simon Coenen, *DOOM Eternal — Graphics Study*. https://simoncoenen.com/blog/programming/graphics/DoomEternalStudy
26. Godot, `Viewport` shadow atlas quadrants. https://docs.godotengine.org/en/stable/classes/class_viewport.html
27. Bo Li, *Efficient Real-Time Shadow Rendering*, US Patent 11,908,062 B2, Warner Bros. Entertainment, 2024.
28. godot-proposals #2745 (staggered shadow update schedule). https://github.com/godotengine/godot-proposals/issues/2745
29. Roblox, *Future Is Bright* performance comparison. https://roblox.github.io/future-is-bright/compare.html
30. Christoph Peters & Reinhard Klein, *Moment Shadow Mapping*, I3D 2015. https://momentsingraphics.de/I3D2015.html
31. Christoph Peters, Cedrick Münstermann, Nico Wetzstein, Reinhard Klein, *Beyond Hard Shadows: Moment Shadow Maps for Single Scattering, Soft Shadows and Translucent Occluders*, I3D 2016. http://momentsingraphics.de/I3D2016.html ; building on Klehm, Seidel & Eisemann, *Prefiltered Single Scattering*, I3D 2014.
32. wgpu 26.0.1 `wgpu-types/src/features.rs` and `naga/src/valid/expression.rs`; wgpu issues #7332, #4524, #4358 (depth textures + comparison samplers across backends).
33. Microsoft, *Common Techniques to Improve Shadow Depth Maps*. https://learn.microsoft.com/en-us/windows/win32/dxtecharts/common-techniques-to-improve-shadow-depth-maps
34. Michal Valient, *Taking Killzone Shadow Fall Image Quality into the Next Generation*, GDC 2014. https://gdcvault.com/play/1020770/Taking-Killzone-Shadow-Fall-Image

---
---

# Addendum — round two: the shadow-construction problem

Written after the first profiling pass on the real visualizer, which moved the target. Sections
6–13 supersede parts of §2.4 and §3.3 above; §13 lists the corrections explicitly.

## 6. What measurement changed

The v2 doc was written against the synthetic `profile-volumetrics` harness and aimed at
volumetric shading cost. On the real visualizer the picture is different:

- **Volumetric shading is not the problem.** 0.02 ms at realistic density. The analytic
  MIS beam integrand is already cheap. §3.1's cone-proxy rewrite is a nice-to-have, not a fix.
- **Shadow draw *submission* is the problem.** 16 fixture shadow maps × ~2000 opaque draws =
  **~33,000 draws/frame, ~16 ms CPU encode + 9 ms GPU.**
- **Tails, not medians.** Camera motion invalidates the cluster grid *and* every shadow view on
  the same frame.

So the question is no longer "how do we integrate scattering faster." It is **"how do we stop
re-rasterizing the scene once per light per frame."** Everything below is aimed at that.

The most important consequence of re-reading the problem this way: **transmittance
*representation* and shadow *construction* are different problems, and only construction costs
us anything.** Swapping PCF for moment maps, or moment maps for Fourier opacity maps, changes
storage and filtering. It does not remove a single draw call. §9 says this at length because it
is the trap this problem invites.

## 7. The apex invariant — the finding that reframes everything

**A moving head pans and tilts. Its light-emitting point does not move.**

Verified in this codebase. `src-tauri/src/fixtures/layout.rs`:

```rust
pub fn head_world_position(base: [f32; 3], rot: [f64; 3], offset: HeadLayout) -> [f32; 3]
```

`base` is the rig position, `rot` is the fixture's *mounting* orientation, `offset` is the
static head layout within the fixture body. **None of the three is pan or tilt.** Pan/tilt
produces the beam `direction`, which is a separate per-frame quantity. `FixtureCone.position` is
therefore constant across an entire show unless someone edits the rig or flies the truss.

Now look at what the renderer does with that (`gpu.rs::fixture_shadow_matrix`):

```rust
let view = Mat4::look_at_rh(light.position, light.position + direction, up);
let field = (2.0 * light.cos_field...acos())...;      // the *beam* field angle
Mat4::perspective_rh(field, 1.0, far, near) * view
```

The frustum is slaved to `direction` and tightened to the beam angle, so **every pan/tilt
invalidates it** — and `ShadowCacheKey` hashes the matrix bits, so every fixture is dirty every
frame. That is where 33,000 draws come from.

But a depth buffer rendered from a fixed apex is **invariant under pan/tilt**. The scene doesn't
move relative to the light's *position*; only the projection rotates. Pan/tilt is a change of
*lookup direction*, not a change of occlusion. The renderer is recomputing a quantity that
didn't change.

### 7.1 What this unlocks

**Anchor the shadow view to the apex, not the beam.** Give the map a generous fixed FOV and an
orientation snapped to a coarse lattice with hysteresis, so a beam sweeping inside the covered
cone reuses the same map. Invalidate on three events only:

1. beam direction leaves the covered cone (with an `ε_out > ε_in` hysteresis band),
2. the fixture is physically moved (editor action, not per-frame),
3. a caster moves.

This is the standard texel-snapping / constant-extent stability argument from Microsoft's
shadow guidance³³ ("*many techniques give better results when the size of the light's projection
remains constant in every frame*"), applied to orientation instead of translation.

The resolution trade is real and affordable. A 90° map is 4.5× coarser per axis than a 20°
tight map at equal texel count — but you now render it perhaps 1 frame in 20 instead of every
frame, so 256² → 1024² is a 4× texel cost against a ~20× reduction in render frequency.

### 7.2 The correction to §2.4 — caching is back on the table

§2.4 above says, emphatically, *"Do not build the static/dynamic split"*, on the grounds that
every published static-caster cache assumes a static **light**, and our light frustums rotate
every frame. **That reasoning was right and its premise was wrong.** With an apex-anchored view,
the light frustum *is* effectively static, and the entire published caching literature becomes
applicable again:

- DOOM's static-portion caching with dynamic geometry composited on top²⁵
- UE's `r.Shadow.CacheWholeSceneShadows` (measured **14.89 ms → 0.9 ms**, ~16×, for three
  shadow-casting movable point lights)
- HDRP's mixed cached shadows

Our venue geometry (truss, deck, risers, set) is static. Performers and moving set pieces are
not. So the split is: **render the static caster set once per apex-anchored view and keep it;
each frame, blit the cached depth and redraw only dynamic casters.** For a typical show that is
a handful of draws per light per frame instead of ~2000.

This also resolves the tension HDRP documents but cannot fix — that dynamic rescale and caching
fight each other, because changing a cached tile's resolution forces reallocation and
re-render.⁴ Apex-anchored views change resolution only when a fixture changes tier, which is a
slow, hysteretic event, not a per-frame one.

### 7.3 Honest limits

- **Pan/tilt range is nearly spherical** (~540° pan, ~270° tilt), so no single perspective map
  covers everything. In practice a fixture in a cue points into a limited sector, which is why
  hysteretic re-aim beats trying to cover the full sphere with a cube map. Worst case — a beam
  slewing continuously through a wide arc — degenerates to today's behaviour, so the throttle
  of §3.3 still has to exist as the backstop.
- **Fixtures that genuinely translate** (truss lifts, tracking systems) exist and are slow.
  Same invalidation predicate handles them.
- **Shared apexes.** Multiple heads in one fixture body, and fixtures adjacent on a bar, have
  nearly coincident apexes. Sharing one map across them is a further multiplier, and the
  approximation error is bounded by the apex separation over the throw distance. Worth
  measuring before assuming.

## 8. Draw submission — the id Tech 7 merged index buffer

Even with §7, dynamic casters still need submitting, and the general fix is published and
directly implementable on our stack.

**Geffroy & Gneiting, *Rendering the Hellscape of DOOM Eternal*, SIGGRAPH 2020**³⁵. Their
motivation is our sentence: *"Levels built with large amount of instantiated models, up to 15k
in view… **Significant CPU cost to issue draw calls.**"*

The mechanism:

- CPU groups meshes into **Geometry Sets** — *"up to 256 meshes sharing the same PSO."*
- **One culling dispatch per Geometry Set.** The compute shader writes a **merged indirect index
  buffer**, packing **`VertexID` and `InstanceID` into each 32-bit index**, *"correct behavior
  when it comes to HW vertex reuse."*
- **One indexed indirect draw per Geometry Set** — *"effectively draws 256 meshes at once."*
- The vertex shader unpacks the index, uses `InstanceID` to fetch instance data from buffers
  rather than vertex attributes.

Reported: *"Up to 5 ms GPU savings… Similar CPU savings… **Opaque/PreZ draw calls almost
completely disappear.**"* And a tail-relevant detail: *"Culling/Merging shader runs async,
started before shadows… possible culling/draw overlap."*

### 8.1 Why this one survives wgpu, when the alternatives don't

This is the crux, and it is worth being precise because most GPU-driven techniques do **not**
survive:

| mechanism | available to us? |
|---|---|
| compute culling → storage buffer | ✅ core |
| compute writes index buffer + `DrawIndexedIndirect` args | ✅ core (`INDIRECT \| STORAGE` usage) |
| **draw count known on CPU, index count written by GPU** | ✅ **this is the whole trick** |
| `MULTI_DRAW_INDIRECT` | ⚠️ loop-emulated on Metal — **zero CPU saving** |
| `MULTI_DRAW_INDIRECT_COUNT` | ❌ not on Metal |
| mesh shaders | ❌ do not exist in wgpu |
| geometry shader `gl_Layer` | ❌ do not exist in wgpu or Metal |
| VS-written layer index | ❌ not exposed — gfx-rs/wgpu#1475, open since 2019 |
| `Features::MULTIVIEW` | ❌ Vulkan/GL only in wgpu 26 |
| `CLIP_DISTANCES` | ❌ Vulkan/GL only |
| viewport arrays | ⚠️ capped at 16 on Metal anyway |
| `EXPERIMENTAL_RAY_QUERY` | ❌ Vulkan only in 26.0.1 |

The id Tech formulation is uniquely well-matched: **the draw count is a CPU-known constant** (the
number of Geometry Sets) and only the *index count* is GPU-determined, which lives in the
indirect args buffer that compute can write. It needs none of the unavailable features.

And the "same PSO" constraint that forced id into ubershaders **costs us nothing** — a shadow
pass is depth-only, so every opaque caster already shares one pipeline.

**Expected:** ~2000 draws per light collapses to 1–8. Combined with §7's static cache, the
16 ms CPU encode should approach the noise floor.

### 8.2 One atlas, one render pass

Independently of merging: today each shadow map is its own `begin_render_pass` against its own
array layer. Collapse to **one atlas, one pass, `set_viewport`/`set_scissor_rect` per light
tile**. Scissor and viewport are cheap encoder state, not pass boundaries — on Metal, pass setup
is the dominant fixed cost. This is the cheapest single change in the whole addendum.

Note this trades against §3.3's preference for texture arrays (free per-layer mips, no gutter
bleed). If moment shadow maps and prefiltering land, keep arrays and instead use one pass per
*tier* with layered writes; if they don't, the atlas wins. Decide when §3.3 is scheduled, not
before.

### 8.3 The bigger hammer, if ever needed

Nanite renders **all shadow views in one dispatch** — *"the Nanite pipeline gets an array of
views… it can render all shadow maps for every light in the scene, to all of their virtualized
mipmaps at once. **In extreme cases we've seen a 100× speedup compared to individual calls**"*³⁶.
Critically, this does **not** use mesh shaders or layer routing: it is a compute rasterizer doing
**64-bit atomic-min** depth writes into a flat page pool, with per-cluster view tagging and
software scissoring. `TEXTURE_INT64_ATOMIC` and `SHADER_INT64` are both **supported on Metal**
(MSL 3.1+ / 2.3+) in wgpu 26, so this is legal on our stack.

It is also a large project. §7 + §8.1 should get us there for far less; keep this as the
escalation path if 120 simultaneous shadowed fixtures ever becomes the requirement.

## 9. Volumetric shadow representations — the honest negative result

The brief asked whether beams need *geometric* shadow maps, since a shadow that only modulates
media has a lower quality bar. The literature answer is clear and unwelcome:

**Every volumetric transmittance representation is still built by rasterizing casters from the
light.** FOM, Opacity Shadow Maps, Deep Shadow Maps, AVSM, Transmittance Function Maps,
moment-based transmittance — all of them change *storage and reconstruction*, none changes
*construction*. **They buy zero of our 16 ms.**

**Fourier Opacity Mapping** (Jansen & Bavoil, I3D 2010)³⁷ specifically: built in one additive
pass, no sorting, prefilterable, 7 coefficients in two RGBA16F targets. Genuinely elegant. But
its own §7.3 states that ringing is governed by feature size and opacity, and it is characterised
as suitable for *"low opacity and large feature sizes."* The paper's recommended handling for
opaque occluders is **a separate ordinary shadow map, multiplied in** — i.e. the thing we
already have. Hard-edged truss shadows are the worst case for a truncated Fourier basis, and
sharp edges are the product.

**On the Destiny 2 claim in the brief: I could not substantiate it.** Searched Tatarchuk's
*Destiny Rendering Engine* (SIGGRAPH 2013), Whitley's *Destiny Particle Architecture*
(SIGGRAPH 2017), the full GDC Vault Destiny listing, and Advances course indices 2013–2019.
There is no Destiny 2 volumetrics talk, and no Bungie FOM reference. **Verified** production FOM
users are Unreal (translucency self-shadowing — and Epic's own docs say FOM has *"severe ringing
artifacts with more opaque translucent surfaces"*) and Frostbite (sun/particle interaction,
slide 43: *"Used in production now"*). Every verified use is blobby media, never opaque casters.

**The one genuinely light-independent family** — built once, queried by all lights — is voxel
occupancy / distance fields:

- Frostbite's split is the practical shape: voxelize casters once into a frustum-oriented
  extinction clipmap, then per-light ray-march that into a tiny **32³** transmittance volume at
  **0.04 ms per spot** on PS4⁸. **Zero geometry draws per light.** Their stated limit is the
  catch: *"Soft shadows, no sharp details."*
- UE's global distance field⁴⁰ is *"4 clipmaps of 128³"*, *"average cost of maintaining is close
  to 0"*, worst case ~7 ms on teleport, and Epic claims 25–45% faster than conventional shadow
  maps per light. But every SDF/cone-trace technique widens its occlusion filter footprint with
  trace distance **by construction** — the exact opposite of a gobo edge staying crisp at 12 m.

**Verdict:** these belong as a **far-field / low-priority tier** and as the occlusion term for
the *ambient haze* march, where softness is invisible and arguably correct. They cannot carry
the near-field beam cut. Since our geometry is static, the voxelization is a one-time build,
which makes the tier nearly free to add.

For completeness, the two techniques that *would* give exact edges:
**Billeter, Sintorn & Assarsson, *Real-Time Volumetric Shadows using Polygonal Light Volumes*,
HPG 2010**³⁸ extrudes the shadow map into a light hull and integrates airlight analytically at
lit↔unlit transitions — *"razor-sharp shadow boundaries"*, order-independent additive blending,
cost *"largely independent of the number of triangles."* Highest quality ceiling in the corpus,
and it is plain rasterization, fully available to us. Its risk is overdraw: 40 converging beams
means 40 overlapping blended hulls, which relocates the p95 spike rather than removing it.
**Peters et al., I3D 2016 / JCGT 2017**³¹ computes single scattering in *one* texture lookup via
prefiltered moments — but its epipolar rectification is **per-light and per-camera** and
formulated for a *directional* light; 120 cones means 120 rectifications per frame. Ruled out for
the same reason as epipolar sampling.

## 10. Analytic single scattering — optimises what we are not paying for

Follow-up on §2.2. The full Pegoraro line resolves the question of whether a closed form can
replace the MIS estimator, and the answer is no — for a quantitative reason worth recording.

**Pegoraro, Schott & Parker, EGSR 2010**¹⁴ solve the airlight integral in closed form for
arbitrary *azimuthally-symmetric* phase functions and punctual lights with arbitrary 1-D angular
distributions. Their Table 4 (GTX 280, 768×768, chromatic):

| scattering mode | closed form |
|---|---|
| isotropic | 315 fps |
| Rayleigh | 44.7 fps |
| **spotlight (`cos¹⁰`, 1 term)** | **3.53 fps** |
| light ball (order 10, 6 terms) | 1.71 fps |

Their §6: cost is *"a supra-linear function of the order of the angular distributions."* A
`cos¹⁰` spot — **softer than ours** — costs ~280× an isotropic Sun-et-al. lookup. Their own
Fig. 6 is literally a concert stage lit by spotlights, at 1.71 fps. The earlier **dual
formulation** (Graphics Interface 2009)³⁹, a Taylor-truncated approximation rather than a closed
form, is the only member of the family that is real-time for spots: 39 fps for a two-lobed
spotlight on 2009 hardware.

Two structural blockers beyond cost:

1. **A gobo is a 2-D angular distribution.** Pegoraro §6 lists 2-D light distributions as future
   work, and I found no paper that did it. A polynomial/Legendre basis cannot represent a
   high-frequency image with hard edges; a hard-edged iris is a step function in `cos θ`, so a
   polynomial fit gives **Gibbs ringing exactly at the beam edge** — the one artifact we cannot
   ship.
2. **Closed forms assume unoccluded segments.** Pegoraro §6 again: handling volumetric shadows
   means *"partitioning the domain of integration so as to exclude occluded intervals."*
   Analytic scattering doesn't remove the shadow problem, it converts it into a shadow-volume
   interval problem.

**The one idea worth taking, and it is free.** Keep the hard cone boundary *outside* the
integral: analytically clip the view ray against the cone quadric (which `haze.wgsl` already
does), then evaluate scattering only on the clipped interval with a smooth profile inside.
Exact edge, zero ringing, and it composes with Sun et al.'s `F(u,v)` — a 64×64 LUT, <2% error —
for the homogeneous un-goboed case. We already have the clipping; the LUT is a small optional
optimisation of a cost we are not paying.

Also newly published and worth a line: **KT, Shah & Narayanan, *Linearly Transformed Spherical
Distributions for Interactive Single Scattering with Area Lights*, Eurographics 2025**⁴¹ — the
only work applying LTC machinery to participating media. Its own abstract concedes the base
method is **unshadowed** and *semi*-analytic, with shadows arriving via a Monte Carlo ratio
estimator. Aimed at area lights, not 120 cones. Not adoptable.

## 11. Tail smoothing

The measured failure is that camera motion invalidates the cluster grid *and* every shadow view
on the same frame. Shipped renderers avoid this structurally, not statistically.

**Convert data-dependent work into constant work.** Three mechanisms, always together:

- **idTech8**⁴²: *"16×16×16 × 6 cascades… **Interleaved updates. Per frame: 1 cascade + 1
  volume.**"* Each cascade refreshes every 6 frames, phase-offset so exactly one is touched per
  frame. It is *structurally impossible* for two to invalidate together. This is the cleanest
  published statement of what we want.
- **Assassin's Creed Shadows**⁴³: a secondary shadow map *"split in 4×4 tiles, time-sliced update
  over 16 frames"*; a fixed **512/1024 probes per frame** budget with a 3-tier greedy priority;
  and **per-cascade round-robin cursors** — *"each cascade has two indices… keeping the last
  updated probe index, so that the next frame the selection can start from the next probe."*
- **A degradation valve on the one tier whose size is camera-dependent.** Same talk: new probes
  are computed at *quarter resolution* *"to **reduce and stabilize** the cost"*, and *"we can
  disable a cascade if the camera moves too much."* That is variance reduction as an explicit
  design goal, which is exactly the p95 framing.

**Quantise camera-derived transforms so small moves produce bit-identical parameters.** This is
Microsoft's texel-snapping argument³³ generalised: snap the cluster-grid origin and split
distances to a lattice with **constant extent**, so slow orbit produces the same grid for many
consecutive frames and the rebuild branch simply doesn't fire. Add hysteresis (`ε_out > ε_in`) so
it doesn't oscillate at a lattice boundary. This supersedes the "camera-quantised cluster cache"
tactical fix by giving it a principled form — and §3.2's eventual GPU rebuild makes the question
moot, since a per-frame rebuild has no invalidation event at all.

**Anchor what can be anchored in light space.** §7 already does this for shadows and is the
single biggest decorrelation win available: a spotlight's shadow map is a function of the light
and the scene, not the camera. If our shadow views invalidate on camera move, they are
camera-fitted, which is right for a sun cascade and wrong for 120 local spots.

**Async compute is not available to us.** Verified against the vendored sources: wgpu exposes
exactly one queue (`Adapter::request_device` returns one `Queue`); the Metal backend creates one
`MTLCommandQueue`; and compute encoders are created with the default *serial* dispatch type
(`MTLDispatchTypeConcurrent` is never set), so even independent dispatches within one pass
serialize. No path in 26 or 29. For scale, this costs us roughly what it earns others: idTech8
report *"around 0.5 ms on Series X and PS5"*; AC Shadows measured 0.91 ms async vs 1.24 ms
serial for probe updates. Worth knowing, not worth waiting for — and it is a *mean* win, not a
tail win.

**Pipeline caching is also unavailable, and probably irrelevant.** `Features::PIPELINE_CACHE` is
documented Vulkan-only, *"Unimplemented Platforms: DX12, Metal"*, in both 26 and 29. The Metal
HAL detects `MTLBinaryArchive` support into a field it never reads. But Metal compiles at
pipeline-creation time, not first draw, so as long as every pipeline is created at load (on a
worker thread — `Device` is `Send + Sync`) this is not a spike source. Deprioritise.

**Measure the right thing first.** A 12–28× max/p95 measured *present-to-present* with flat GPU
timestamps is a frame-pacing / buffer-stuffing artifact, not a renderer problem — the standard
reference is Google's Frame Pacing documentation on short- and long-frame jank⁴⁴, and Ladavac's
*The Elusive Frame Timing*⁴⁵. Split the measurement into GPU pass time (timestamp queries —
`TIMESTAMP_QUERY` and `TIMESTAMP_QUERY_INSIDE_ENCODERS` are both supported on Metal in wgpu 26),
CPU frame-build time, and present-to-present, before optimising against the wrong number.

## 12. Newer directions — honest verdicts

**UE5 MegaLights**⁴⁶ is the only genuine advance in this space since 2015, and it is worth
reading even though we cannot ship it. Epic's own words settle a question the v2 doc left open:

> *"There's only so much what we can cache before shadow cache invalidation caused by light,
> camera or object movement breaks performance… in MegaLights demo we have bots flying around
> the scene creating **100s of moving lights, which completely break any caching schemes**…
> enabling VSM fills the max allocation size of 4 GB for the shadow map virtual page cache and
> still isn't able to fit all the required lights causing various artifacts."*

**That is a definitive answer on VSM for our workload: do not port it.** Epic's response was to
abandon per-light shadow maps entirely for stochastic light sampling with a fixed ray budget —
PS5, 1080p, **941 area lights all casting shadows, 5.51 ms total**, of which volumetric fog plus
translucency is ~1.43 ms. It requires hardware ray tracing, which wgpu 26 does not expose on
Metal (`EXPERIMENTAL_RAY_QUERY` is Vulkan-only; Metal acceleration structures landed in wgpu 29
and are still experimental).

The *principle* transfers without RT, though, and is the most interesting long-range idea here:
**stochastically select which lights each froxel shadows against, with a fixed budget, and
denoise** — substituting an atlas lookup for the ray trace. The p95 payoff is that per-froxel
cost becomes fixed instead of proportional to beam overlap, which attacks the beam-convergence
frame that is almost certainly our true max. The risk is that stochastic selection plus denoise
softens edges, and Epic's own notes admit multi-neighbour gathering gives *"softer, less detailed
lighting."* Prototype before believing.

**ReSTIR for volumes**⁴⁷ — paper-ware for us, though not for the reason assumed. The light-count
hypothesis is refutable: Cyberpunk 2077's shipped ReSTIR ran at *"up to 250 lights"*, and
published baselines run at 32. The real blockers are that (a) we have no ray tracing, (b) our
visibility is *analytic* — a 5° cone is a closed-form intersection, and replacing a zero-variance
deterministic answer with a stochastic estimator plus denoiser is strictly worse **specifically
on p95**, and (c) the volumetric ReSTIR paper reports 55–142 ms on an RTX 3090 and its own
conclusion suggests *"previsualization for offline rendering."* The course notes also warn
directly against the obvious froxel adaptation: *"a reservoir is not valid over, say, an entire
voxel… it is very difficult to avoid adding bias."*

**Neural transmittance / radiance caching** — dead on arrival, for a hardware reason. Neural
Radiance Caching is explicitly tensor-core-bound (*"the neural radiance cache employs fixed
function hardware (the GPU tensor cores)"*), ~2.6 ms overhead on an RTX 3090, CUDA-only.
**M3 Max has no GPU matrix units** — Apple introduced per-core Neural Accelerators with M5.
Metal 4 has the API (Shader ML, `MTLTensor`); wgpu cannot reach it. And the deeper objection
stands regardless: transmittance through homogeneous haze along a ray through an analytic cone
is closed-form. Training a network to approximate a function evaluable in a dozen ALU ops is
the wrong tool at any hardware level.

**Radiance caching for media** — our suspicion is confirmed by the cache authors themselves.
Jarosz et al.⁴⁸ rest the whole method on *"the distribution of inscattered radiance is often
**smooth**"*, partition the cache by frequency so the high-frequency direct term gets a *dense*
cache (no savings where we need it), and state the fatal limitation outright: the error metric
*"is unable to adapt sample density at **volumetric shadow boundaries**"* because the derivation
*"assume[s] constant visibility."* A beam edge *is* a visibility discontinuity. Every shipped
cache repeats the contract — Unity's APV bricks are 1–27 m, Unreal caches 2-band SH. **Cache the
low-frequency terms (multi-scatter glow, venue bounce, ambient); never the direct beam.**

**One cheap idea worth stealing regardless.** Patry's *Ghost of Tsushima* volumetrics⁴⁹ (froxel
grid, ~0.5 ms on a base PS4) stores **radiance ÷ opacity and re-applies opacity per-pixel at
composite** as a density-aliasing fix — directly applicable to beam-vs-froxel aliasing if §3.4
ever lands. They also use flat bit-array light masks per cluster, independently confirming
§3.2's choice.

## 13. Ranked verdict

Ordered by expected effect on the **measured** numbers.

| # | change | attacks | expected | risk |
|---|---|---|---|---|
| 1 | **Apex-anchored shadow views** (§7) — generous FOV, snapped orientation, hysteretic invalidation | 33k draws; camera/light invalidation correlation | most fixtures stop re-rendering most frames | resolution loss; wide slews degenerate to today |
| 2 | **Static caster cache per anchored view** (§7.2) — cache static depth, redraw only dynamic | the residual per-frame draws | ~2000 → a handful per light (UE measured ~16×) | needs a static/dynamic caster classification |
| 3 | **One atlas, one pass, per-light scissor** (§8.2) | 16 `begin_render_pass` → 1 | cheapest change here; Metal pass setup is the fixed cost | trades against array-mip benefits of §3.3 |
| 4 | **id Tech merged index buffer** (§8) — compute cull → packed `VertexID\|InstanceID` → one indirect draw per Geometry Set | draw submission generally | id measured ~5 ms GPU and similar CPU | needs vertex pulling + instance data in buffers |
| 5 | **Decorrelated update schedule** (§11) — per-subsystem budget + round-robin cursor + fixed phase offsets | p95/max | idTech8's "1 cascade + 1 volume per frame" makes collisions impossible | none; strictly a scheduling change |
| 6 | **Quantise + hysteresis on camera-derived params** (§11) | cluster rebuild spikes | slow orbit stops triggering rebuilds at all | superseded if §3.2 GPU rebuild lands |
| 7 | **Split the measurement** (§11) — GPU timestamps vs CPU build vs present-to-present | knowing whether 4–6 are even the right target | cheap, and may reframe everything | none |
| 8 | Moment shadow maps (§3.3) | filter cost, not draw cost | real but secondary now | light bleeding on near/far pairs |
| 9 | Voxel/SDF far-field occlusion tier (§9) | far-field shadows, ambient march | light-independent, one-time build since geometry is static | soft only — cannot carry near-field beams |
| 10 | Cone-proxy beam rasterization (§3.1) | volumetric shading (0.02 ms) | negligible now | was #2 in v2; demoted by measurement |
| — | Nanite-style compute rasterizer (§8.3) | all views in one dispatch | escalation path only | large project |
| ✗ | VSM, FOM for opaque casters, closed-form spot scattering, ReSTIR, neural, epipolar, DPSM | — | ruled out with citations above | — |

### Corrections to the earlier sections

- **§2.4 "Do not build the static/dynamic split"** — wrong premise. It assumed light frustums
  rotate every frame. They only do because `fixture_shadow_matrix` slaves the frustum to the
  beam direction. With §7's apex anchoring, the split is valid and is item #2 above.
- **§3.1 cone-proxy beam rasterization** was ranked as the biggest win. Measurement demotes it:
  volumetric shading is 0.02 ms. It remains correct and worth doing for the sharpness and
  cull-deletion arguments, but it is not a performance fix.
- **§3.3's per-frame shadow budget N=16** is still right as a backstop, but it is now the
  *fallback* for fixtures whose beams slew out of their anchored cone, not the primary
  mechanism. Its priority function should score angular distance from the anchored view's
  centre, not raw motion.
- **§3.5's async compute suggestion** — unavailable. wgpu exposes one queue and the Metal
  backend uses serial dispatch encoders. Remove it from the plan.

### Additional references

35. Jean Geffroy & Axel Gneiting, *Rendering the Hellscape of DOOM Eternal*, SIGGRAPH 2020 Advances in Real-Time Rendering. https://advances.realtimerendering.com/s2020/RenderingDoomEternal.pdf
36. Brian Karis, Rune Stubbe, Graham Wihlidal, *A Deep Dive into Nanite Virtualized Geometry*, SIGGRAPH 2021 Advances. https://advances.realtimerendering.com/s2021/Karis_Nanite_SIGGRAPH_Advances_2021_final.pdf
37. Jon Jansen & Louis Bavoil, *Fourier Opacity Mapping*, I3D 2010. https://volumetricshadows.wordpress.com/wp-content/uploads/2011/06/fourier-opacity-mapping.pdf
38. Markus Billeter, Erik Sintorn, Ulf Assarsson, *Real-Time Volumetric Shadows using Polygonal Light Volumes*, HPG 2010. https://www.cse.chalmers.se/~uffe/volumetricshadows.pdf
39. Vincent Pegoraro, Mathias Schott, Steven Parker, *An Analytical Approach to Single Scattering for Anisotropic Media and Light Distributions*, Graphics Interface 2009. http://www.sci.utah.edu/~vpegorar/research/2009_GI.pdf
40. Daniel Wright, *Dynamic Occlusion with Signed Distance Fields*, SIGGRAPH 2015 Advances. https://advances.realtimerendering.com/s2015/DynamicOcclusionWithSignedDistanceFields.pdf
41. Aakash KT, Ishaan Shah, P. J. Narayanan, *Linearly Transformed Spherical Distributions for Interactive Single Scattering with Area Lights*, CGF 44(2), Eurographics 2025. https://doi.org/10.1111/cgf.70049
42. Tiago Sousa, *Fast as Hell: idTech8 Global Illumination*, SIGGRAPH 2025 Advances. https://advances.realtimerendering.com/s2025/content/SOUSA_SIGGRAPH_2025_Final.pdf
43. Luc Leblanc & Melino Conte, *Ray Tracing the World of Assassin's Creed Shadows*, SIGGRAPH 2025 Advances. https://advances.realtimerendering.com/s2025/
44. Google, *Android Frame Pacing (Swappy)*. https://developer.android.com/games/sdk/frame-pacing
45. Alen Ladavac, *The Elusive Frame Timing*. https://medium.com/@alen.ladavac/the-elusive-frame-timing-168f899aec92
46. Krzysztof Narkowicz & Tiago Costa, *MegaLights: Stochastic Direct Lighting in Unreal Engine 5*, SIGGRAPH 2025 Advances. https://advances.realtimerendering.com/s2025/content/MegaLights_Stochastic_Direct_Lighting_2025.pdf
47. Daqi Lin, Chris Wyman, Cem Yuksel, *Fast Volume Rendering with Spatiotemporal Reservoir Resampling*, ACM TOG 40(6), SIGGRAPH Asia 2021. https://dqlin.xyz/pubs/2021-sa-VOR/ ; Wyman et al., *A Gentle Introduction to ReSTIR*, SIGGRAPH 2023 Courses. https://intro-to-restir.cwyman.org/
48. Wojciech Jarosz, Craig Donner, Matthias Zwicker, Henrik Wann Jensen, *Radiance Caching for Participating Media*, ACM TOG 27(1), 2008. https://cs.dartmouth.edu/~wjarosz/publications/jarosz08radiance.pdf
49. Jasmin Patry, *Real-Time Samurai Cinema: Lighting, Atmosphere and Tonemapping in Ghost of Tsushima*, SIGGRAPH 2021 Advances. https://advances.realtimerendering.com/s2021/jpatry_advances2021/index.html
50. Ola Olsson, Markus Billeter, Erik Sintorn, Viktor Kämpe, Ulf Assarsson, *More Efficient Virtual Shadow Maps for Many Lights*, IEEE TVCG 21(5), 2015. https://www.cse.chalmers.se/~d00sint/more_efficient/clustered_shadows_tvcg.pdf
51. Daqi Lin, Chris Wyman, Cem Yuksel et al., *Many-Light Rendering Using ReSTIR-Sampled Shadow Maps*, CGF, Eurographics 2025. https://graphics.cs.utah.edu/research/projects/restir-shadow-maps/restir-shadow-maps-eg2025.pdf

---
---

# Addendum — round three: the previz category, and the aperture offset

Two questions: how does Depence render this workload, and does the aperture-vs-pivot offset break
§7's apex invariant. The second answer is the more surprising one.

Sourcing note: Depence is closed commercial software and **Syncronorm has published nothing
technical** — no whitepaper, no dev talk, no conference presentation, no patents (Google Patents
has Syncronorm as assignee of nothing). That absence is itself a finding. Everything below is
labelled **[V]** verified from an official manual/spec/source, **[I]** inferred from published
behaviour or requirements, or **[?]** unknown. Their GitBook manual leaks considerably more
architecture than their marketing does, including a direct contradiction of it.

## 14. What the previz category actually does

### 14.1 Depence is a conventional rasterizer

**[V] Proprietary in-house engine**, not Unreal/Unity/V-Ray: *"The Depence rendering engine was
specially built to handle the massive requirements of simulating multimedia shows."*⁵²

**[V] Marketing says ray tracing; the manual says otherwise.** Marketing: *"Physical based
real-time raytraced lighting beams."* Manual, verbatim: *"Depence is designed for real time
performance. **Its renderer is based on rasterization.** … the renderer **does not automatically
trace rays** into the scene."*⁵³ **[I]** "Raytraced beams" is marketing for per-beam raymarching
in a pixel shader. The reflection stack settles it — three hand-configured sources (planar
reflectors, *"the entire scene must be rendered again (in mirror image)! Therefore, these are very
costly!"*, one global probe, HDRI). Nobody with hardware RT ships that. R4's headline "Beam
Reflections" is *"one bounce"* for spotlights against designated mirrors — a bounded re-projection,
not traced rays.

**[V] No DLSS, FSR, XeSS, frame generation, hardware RT, or path tracing anywhere** in docs or
marketing. The only resolution lever is *"In critical cases, you can reduce the resolution for
fullscreen mode."*

**[V] The single most informative sentence Syncronorm has published**, from their system
requirements: *"Depence **does not scale with multiple GPUs** … **Professional GPUs like the RTX
PRO 6000 are less suitable for Depence.**"*⁵⁴ **[I]** An RTX PRO 6000 has more SMs, more RT cores,
more VRAM and ECC than a 5090; what it has less of is sustained boost clock. Preferring the 5090
means the bottleneck is **raster/shader throughput at high clocks** — not RT cores, not VRAM, not
compute density. Combined with "no multi-GPU scaling" and their stated preference for *"CPUs with
high single-core performance"*, this describes a single-threaded-submission rasterizer with heavy
pixel-shader work. **We are not behind the category leader on rendering technology.** Their moat is
a hand-curated fixture library, not an exotic renderer.

### 14.2 Nobody's realtime beam is a froxel volume — including Unreal's own previz

This is the headline result of the survey, and it independently validates §3.1.

**[I, high confidence]** Depence rasterizes a per-fixture cone proxy and raymarches it. The tell is
their published cost list: *"Light sources that **shine directly into the camera**"* is named as a
top performance cost.⁵⁵ In a camera-frustum froxel grid, a light aimed at the camera costs exactly
what one aimed away costs — grid resolution is fixed and contribution is gathered per froxel. Cost
becomes orientation-dependent only when you rasterize a bounding cone and march in the fragment
shader, where a beam aimed at the lens goes from a small on-screen cone to a full-screen quad.

**Capture states the identical failure mode explicitly [V]:** *"Fixture focus (when smoke is
present) — fixtures focused into the camera effectively affect the entire screen **as compared to
what is normally a small cone**."*⁵⁶ Two independent codebases, same signature.

Corroborating for Depence: **[V]** *"Volumetric beams are generated **per fixture**"* with a
per-fixture Always/Render/Never toggle — a froxel volume has no natural per-fixture beam switch;
**[V]** a global **Max Spotlight Range** clamp described as a performance lever, which is a
raymarch length bound and is meaningless in a froxel volume; **[V]** beam and surface projection
are independently disableable, i.e. the same three-part decomposition (raymarched in-air beam,
projected gobo from a real spot light, cosmetic lens glow) that Unreal's DMX fixtures use.

**Unreal's own DMX previz bypasses UE Volumetric Fog for beams** and is the richest public source
in the space. From the plugin source **[V]**: `ADMXFixtureActor` holds a `StaticMeshBeam` +
`DynamicMaterialBeam`, a real `USpotLightComponent` whose *light function* paints the gobo, and a
cosmetic lens. Quality is literally raymarch step size — `Low 4.0 / Medium 2.0 / High 1.0 /
Ultra 0.33` distance units between samples — with `LightCastShadow = false` **by default**.⁵⁷
Their published scale: the DMX Previs sample (Epic × Moment Factory) runs **768 fixtures, ~182 of
them moving heads with individually raymarched beams**, 7368 DMX channels across 15 universes,
driven live from a grandMA2.⁵⁸ Their #1 performance lever is beam length — the same lever as
Depence's Max Spotlight Range.

And Epic's own comment in `VolumetricFog.cpp` names the assumption that our workload violates
**[V]**: each shadowed light costs a dedicated raster pass, *"Not many lights cast shadow so that
is acceptable."*

### 14.3 The shared mitigation vocabulary

The same small primitive set appears independently in three-plus codebases, which is good evidence
it is the right set:

- **fixed step size as the quality knob** (UE 4.0→0.33; grandMA3's No Beam/Line/Standard/High/High Fancy)
- **zoom-adaptive sample count** — UE's beam material *"dynamically adjusts the volumetric beam
  shader sample counts as a function of the live DMX Zoom angle. The wider the beam is, the lower
  the sample count … the narrower the beam, the higher the sample count for increased beam
  sharpness."* **[V]** This is §3.1's "scale samples by projected solid angle", already shipping.
- **jitter** to hide step banding (UE's `Jitter Scale`)
- **beam-length clamp** — the #1 lever in both Depence and UE
- **render beams at reduced resolution and composite** — grandMA3 exposes *"Light Scale"*
  independent of *"Render Scale"* **[V]**; Capture has adaptive quality. We already do this
  (`haze_resolution`).
- **collapse multi-emitter fixtures** — grandMA3's *"Single Beam Dynamic Gobo"* (*"takes the output
  of the emitters and creates a 'virtual dynamic gobo'"*) **[V]**, Capture's *"Multi-aperture
  rendering: Simplified"*. An LOD we do not have and should consider.
- **shadows opt-in, or tiny** (below)

### 14.4 The shadow number

**[V] Depence defaults to 256×256 per shadow-casting light**, spot and omni separately, with the
manual warning these *"should only be changed in special cases and with few light sources."*⁵⁹
Everything casts by default; the opt-out is **per object** (*"Deactivate the 'Cast Shadow' property
for these models"*), not per light. There is no documented cap on shadow-casting fixture count.

Three points on the same tradeoff: Depence ships everything shadow-casting at 256²; grandMA3 ships
a per-fixture None→Very High ladder, **off by default**; UE ships DMX fixtures with
`LightCastShadow = false`. None of them is a clever algorithm — the category's answer to hundreds
of shadowed fixtures is **make each map tiny and atlas it**.

**We already use 256²** (`FIXTURE_SHADOW_SIZE = 256`). So we are at the industry-standard
resolution and still choking — which points even harder at §8's conclusion that our problem is
**draw submission, not shadow resolution**. Depence renders far more shadow maps than we do, at
our resolution, on a single-threaded-submission rasterizer. The difference is what they submit per
map, not how big the map is.

**[?]** Whether Depence's beams receive volumetric shadows at all is unverified. **[I, weak]** at
256² any in-beam shadowing would be very soft; I would not build on this inference.

### 14.5 Depence does not use GDTF

**[V]** Stated twice: *"Depence doesn't use the gdtf file format for the fixtures"*; MVR export
emits *"dummy fixtures"* only.⁶⁰ They pay a permanent content-ops cost (a public "Fixture Request"
form, a cloud-delivered curated library) for control over asset quality. Worth knowing as a
strategic fork, because §16 argues we should go the other way.

## 15. The aperture offset — the correction, corrected

Julian's physical note is right: **the emitting aperture is offset from the pan/tilt pivot**, by
55–330 mm depending on fixture class. Measured from manufacturer dimensional drawings⁶¹ (Martin
publishes the tilt-axis callout explicitly; Clay Paky publishes head swing diameter):

| class | tilt-axis → lens face | example |
|---|---|---|
| LED panel | 56 mm | Ayrton MagicPanel FX |
| compact LED wash | 73–112 mm | Martin MAC Aura XB / XIP |
| beam | 157–250 mm | Clay Paky Sharpy / Sharpy Plus |
| mid profile | 238–244 mm | Ayrton Khamsin / Ghibli |
| flagship profile | 297–311 mm | MAC Encore Performance, MAC Viper |

(Also note heads are **not** balanced about the tilt axis — splits run 41/59 to 56/44, and the bias
direction follows the mass, so `head_length / 2` is a ±10–20% estimator of the offset, not a
correct one. And the *larger* offset is the pan→tilt yoke height, 249–446 mm, which is a different
quantity not to conflate with this one.)

### 15.1 But the consequence for §7 is nil, and here is why

The correction assumed the apex "orbits the pivot on a small sphere," weakening the invariant. It
does orbit — but **not arbitrarily. The lens sits *on* the optical axis, which passes through the
tilt axis.** So:

```
aperture  A = P + d·û          (P = pivot, û = beam direction, d = fixed offset)
beam ray:   A + s·û  =  P + (d + s)·û
```

**The two rays are the same line.** Sweep the head 90° and the aperture traces a `d`-radius arc,
but at every instant the emitted ray is collinear with the pivot-origin ray. The offset is a
*reparameterisation along the beam*, not a displacement off it.

**Apex-anchoring therefore survives completely intact — it is stronger than §7 claimed, not
weaker.** For any point W *on* the beam axis, the segment `A→W` is a strict subset of `P→W`, so
pivot-origin visibility is conservative-and-exact: if W is visible from the pivot it is visible
from the aperture, and the only possible disagreement is an occluder inside `[P, A]` — which is the
fixture's own head. The renderer **already excludes fixture bodies from casting into their own
cone** (`draw_range(..., false)`, with the comment *"A luminaire's body sits at the apex of its own
cone and would otherwise shadow every sample"*). The one error case is already defined away.

For off-axis points within the cone the two rays differ, converging at W. Computed:

- **Maximum ray separation** is `d·sin θ` at the light end, tapering to zero at the receiver:
  0.6–5.4 cm across the whole `d` × cone-angle range.
- **Shadow-edge displacement** on the receiver, for a 10 cm occluder at mid-throw:
  **0.2–1.1 cm** at 5–20 m throw. Large only for occluders within ~1–2 m of the fixture — i.e. its
  own body and immediate neighbours, already excluded.

So Julian's "30 cm apex error at 8 m throw" framing has the wrong denominator: the error is not
`d/R` angular, because the offset is along the ray rather than across it.

If we ever want it exactly zero at the anchor: put the shadow view's projection centre at
`P + d·û_anchor` rather than at `P`. Error is then zero at the anchor orientation and bounded by
the hysteresis band. It costs one vector add.

### 15.2 What the offset *does* cost

The offset is real and worth modelling — just not for shadows. In descending visibility:

1. **Apparent source position (dominant, and it is a *look* bug, not a perf bug).** The lens flare
   and beam root are misplaced by up to `d` in world space, and the error scales with distance to
   the **camera**, not the throw. At 1080p / 40° hFOV, a `d = 0.311` fixture misplaces the flare by
   **171 px at 5 m, 86 px at 10 m, 43 px at 20 m**. Over a 90° tilt sweep the aperture travels
   `(π/2)·d` ≈ **0.49 m of arc** — a pivot-anchored flare visibly *fails to swing* while the head
   does. This is the artifact a lighting designer reads as "wrong" even when beams land correctly.
2. **Near-field inverse-square.** True range is `R − d`, so a pivot origin under-lights by
   `(R/(R−d))²`: **+40% at 2 m, +13.7% at 5 m, +6.5% at 10 m, +1.3% at 50 m.** Matters below ~5 m.
3. **Beam root occlusion.** A pivot-origin beam *starts inside the head mesh* — which GDTF
   explicitly forbids: *"The origin of the Geometry Type 'Beam' should not be covered by any faces
   of other geometries in order to not block the rendered beam."* For a volumetric integrator that
   is the difference between a clean root and a ~0.3 m plug of self-occluded haze at the head.
4. **Cone width.** `d·tan(θ/2)` extra radius — 5 cm on a 3.6 m pool. Ignore it.
5. **Multi-emitter heads — the only case with real *angular* error.** Array cells are genuinely
   *off* the optical axis, so collapsing them to one origin **does** change per-cell direction. An
   Ayrton MagicPanel FX has 25 cells over a 391 mm square; corner cells sit ±195 mm off-axis, giving
   **2.2° of per-cell direction error at 5 m** — and the fan/matrix effect, which is the entire
   point of the fixture, collapses.

**Item 5 is the one to fix**, because it is the only place the offset changes where light *lands*
rather than where it appears to come from. And it lands directly on §16.

## 16. luma's fixture model — what is actually missing

I checked. The finding is not the one the correction anticipated.

**`head_world_position` is not a pan/tilt model and was never meant to be.** Reading
`compute_head_offsets` (`src-tauri/src/fixtures/layout.rs`), it lays heads out on a **grid across
the fixture's physical face** — `width × height` divided into `layout_w × layout_h` cells, mapping
QLC+ `<Head>` elements to cell centres. That is a **multi-cell face layout** for pixel bars,
blinders and matrix panels. It answers "where is cell *i* on this fixture's face, given the
fixture's rig position and mounting orientation," and for that question it is correct.

So:

- **There is no pan/tilt pivot, yoke height, or aperture offset concept anywhere in the model.**
  Not wrong — absent.
- **`FixtureCone.position` is consequently pan/tilt-invariant in the code as written**, which is
  what §7 relies on. The apex invariant holds today *by omission*, and §15.1 shows it also holds
  after the physics is fixed. Both paths lead to the same place, which is a comfortable position
  to be in.
- **The real bug is item 5 of §15.2**: for a multi-cell fixture that also pans and tilts, the cells
  are laid out on the static fixture face and never swing with the head. A MagicPanel's fan effect
  cannot be represented. That is a correctness gap in where light *lands*.

### 16.1 The conflation to avoid when fixing it

Two genuinely different quantities are being asked of one concept, and fixing them into one
function would be the wrong move:

- **Rig-layout position** (static): where a head sits in the rig. This is what pattern space wants
  — the PCA-UV work needs the fixture's *place in the rig*, not where its lens happens to be
  pointing this frame. Animating this would make UV space swim as heads move, which is exactly
  wrong.
- **Emission origin** (animated): `pivot + d·û`, plus per-cell offsets rotated by the head's
  articulation. This is what the renderer wants for the beam apex, the lens flare, and the near-field
  falloff.

They must stay separate functions with separate names. `head_world_position` already answers the
first correctly; the second does not exist yet. Note also the existing memo about a
`head_world_position` axis bug blocking PCA-UV — that is a bug in the *first* quantity and is
independent of everything here.

### 16.2 Model it as a geometry chain, not a fudge

GDTF already mandates exactly the structure needed, as nested parent-relative `Position` matrices
**[V]**: *"Use the Geometry Type 'Beam' to describe the position of the fixture's light output
(**usually the position of the lens**) and not the position of the light source inside the
device"*, and *"The offset is defined by the geometry and has to be related to its **parent
geometry**."*⁶² The mandated tree is:

```
Base → Axis(Yoke, pan about Z) → Axis(Head, tilt about X) → Beam
```

so the `Beam` node's `Position` translation **is** the tilt-pivot→lens vector. Adopting that chain
means GDTF import becomes a straight read of `Position` matrices rather than a conversion, and we
get gobo bitmaps, per-facet prism transforms, LOD meshes and a `PrimitiveType` mesh-free fallback
along with it. Depence went the other way and pays a permanent content-ops cost for it (§14.5).

Traps if we do **[V]**: GDTF stores 4×4 row-major but defines the matrix column-major, so
translation is the fourth *column*; MVR uses 4×3 with translation as the last stored triple; GDTF is
**metres** while MVR is **millimetres**; and the spec's emission-axis text says local −Z *"(and
Y-up)"* inside an otherwise Z-up frame, which is a known wart — assert on it in an importer rather
than trusting the text.

Ship-before-per-fixture-data defaults (metres), from the measured table:

```
yoke_height (pan→tilt)   panel 0.45  wash 0.25  beam 0.34  spot 0.43  large profile 0.43
beam_offset (tilt→lens)  panel 0.06  wash 0.10  beam 0.20  spot 0.27  large profile 0.31
```

## 17. What changes

**Nothing in the plan.** §7's apex anchoring, §8's draw-submission work, and the §13 ranking all
stand unchanged — §15.1 strengthens the apex argument rather than weakening it.

Three things are **added**, all small and none on the shadow critical path:

| add | why | where it belongs |
|---|---|---|
| Beam apex at `pivot + d·û`, per-class default `d` | fixes lens-flare swing (171 px at 5 m), near-field falloff, and the beam-root-inside-the-head plug GDTF forbids | fixture model + `FixtureCone` construction; independent of the renderer work |
| Articulated per-cell offsets for multi-emitter heads | the only case where the offset changes where light *lands* (2.2° per-cell at 5 m) | fixture model; a correctness bug, not an optimisation |
| Zoom-adaptive beam sample count | industry-standard, shipping in UE's DMX plugin: narrow beam → more samples | already proposed in §3.1; this is external confirmation |

And two facts worth carrying:

- **We are at the category's shadow resolution already** (256², same as Depence's default). Our
  problem is submission, not resolution — §14.4.
- **No shipping previz tool uses a froxel volume for beams**, including Unreal's own. §3.1's
  cone-proxy direction is the category consensus, and §2.1's rejection of froxel beams is
  independently confirmed by every product in the space. That does not re-promote §3.1 on
  performance grounds — volumetric shading is still 0.02 ms — but it settles the architecture
  question.

One thing to note as unresolved: **"Depence 3" does not exist.** The lineage is Depence² → R3
(2023) → R4 (April 2025), incremental releases on the same engine with no "next-gen renderer"
announcement **[V]**. If someone is holding out for a Depence rewrite to benchmark against, there
isn't one.

### Additional references

52. Syncronorm, *Depence — visualization overview*. https://www.syncronorm.com/products/depence2/visualization/overview
53. Syncronorm, *Depence manual — Reflections*. https://help.depence.com/depence-tips-and-tricks/reflections.md (contrast with the marketing claim at https://www.syncronorm.com/products/depence2/visualization/lighting)
54. Syncronorm, *Depence System Requirements*. https://help.depence.com/depence-getting-started/depence-system-requirements.md
55. Syncronorm, *Depence manual — Performance › Lighting*. https://help.depence.com/performance/lighting.md
56. Capture, *Performance Tuning*. https://www.capture.se/Manual/en-UK/2026/PerformanceTuning.html
57. Epic Games, *DMX Fixtures in Unreal Engine* (archived 5.0 docs). http://web.archive.org/web/20230928085746/https://docs.unrealengine.com/5.0/en-US/dmx-fixtures-in-unreal-engine/ ; plugin source `DMXFixtures/Source/DMXFixtures/`
58. Epic Games, *DMX Previs Sample Project* (with Moment Factory). https://dev.epicgames.com/documentation/en-us/unreal-engine/dmx-previs-sample-project-for-unreal-engine
59. Syncronorm, *Depence manual — Performance › Other optimizations*. https://help.depence.com/performance/other-optimizations.md
60. Syncronorm, *Depence manual — MVR*. https://help.depence.com/depence-construction/mvr.md
61. Manufacturer dimensional drawings: Martin MAC Viper Profile / MAC Encore / MAC Aura XB / XIP (https://www.martin.com), Clay Paky Sharpy & Sharpy Plus (https://www.claypaky.it), Ayrton Ghibli / Khamsin / MagicPanel FX (https://www.ayrton.eu)
62. *GDTF specification*, DataVersion 1.2 / DIN SPEC 15800. https://raw.githubusercontent.com/mvrdevelopment/spec/main/gdtf-spec.md ; *MVR specification*. https://raw.githubusercontent.com/mvrdevelopment/spec/main/mvr-spec.md
63. MA Lighting, *grandMA3 Render Quality*. https://help.malighting.com/grandMA3/2.3/HTML/patch_render_quality.html

## 18. Shadow tenancy — the cliff a real show found at 0:49

The first content-dependent report from a real show: *Club* / *Baddadan*, "perf
goes bad at 0:49–0:51, fine right before". The window is exactly one clip, and
what it does explains the shape of the complaint.

### What the score does

| | 45.0 – 49.66 s | 49.66 – 51.74 s |
|---|---|---|
| clip | `intensity_spikes` | `bass_strobe` |
| selection | `wash`, then `front_led_bars \| wash` | `all` |
| primitives lit | 14 (6 SlimPAR + 2 Tetra Bar) | 30 (adds 16 Focus Spot 5Z moving heads) |
| modulation | one shared beat envelope | per-fixture noise + strobe |
| mean `dimmer_sum` | 2.57 | 2.67 |

The light does not get brighter. The *same* energy is spread over 2.3x as many
emitters, and each emitter is modulated independently instead of together.

Eval is not the cost: `SCORE` in the live toolbar goes 0.01 ms to 0.03 ms. (A
debug build of `luma_lib` reports 0.075 → 0.317 ms for the same window; the
ratio is real, the magnitude is the build.)

### Why 14 → 30 looked like a cliff — and why that is not what happens

**Measured afterwards, and the mechanism below does not fire on this content.**
With `ShadowStats::redrawn_maps` plumbed across the worker seam and read off the
live stage, the count is **0 redrawn maps per frame for every second from 0:40
to 0:54**, the whole window included, and `cpu_cluster_ms` is 0.00 alongside it.
So neither shadow redraw storms nor cluster-grid invalidation explain the
window's cost. Keep reading for why the cliff is nevertheless real in the code,
and why this score cannot reach it.

`MAX_FIXTURE_SHADOWS` is 16.

At 14 lit cones every cone holds a slot, `assign_shadow_slots`' eviction path
never runs, and the ranking never reorders — so no depth map is ever redrawn,
however hard the score blinks them. That is the "before" window, and it is free.

At 30 the eviction path runs every frame. Slots are handed out by priority
*rank*, and `assign_shadow_slots` frees a resident the moment its intensity
reaches zero (the `score > 0.0` filter). Per-fixture modulation therefore
permutes tenancy continuously, and `ShadowCacheKey` is keyed **per slot** by
projection matrix and caster hash — so a cone that returns to a different slot
dirties a depth map that never went stale. A cone's projection does not depend
on how bright it is; only its *slot* changed.

Pinned by `gpu::tests::a_cone_that_blinks_loses_its_shadow_slot_when_the_rig_is_over_the_cap`,
which asserts both halves: under the cap a blink moves nothing, over the cap one
blink reshuffles tenancy.

**Why this score never reaches it.** The eviction filter is `score > 0.0`, which
drops a cone only at *exactly* zero intensity. `bass_strobe` modulates each
fixture continuously but never lands one on a hard zero, so all 30 cones stay
scored, the same 16 keep their slots for the whole clip, and nothing is redrawn.
The cliff needs content that takes a fixture fully dark and brings it back —
a hard shutter, not a modulated dimmer. It is a real trap waiting for such a
score; it is not what 0:49 is.

What the window's cost actually is, then: more lit cones to shade and march,
with shadows and clustering both already paid for and cached. GPU rises from
~0.2 ms to 0.6–1.4 ms typical, with one 10 ms outlier at 49 s.

### The fix, and why it is not landed here

**Ordering note, after the measurement:** apex-anchored shadow views (§7) should
not be scheduled *for this reason*. They would delete a coupling that this
content never exercises. The argument for apex anchoring stands on its own
merits — pan/tilt invalidation — and should be judged there, not here.

`EVICTION_MARGIN` is hysteresis in *score* — it stops two near-equal cones
trading a slot every frame. What is missing is the temporal twin: hysteresis in
*tenancy*, so that a cone which goes dark for a few frames does not lose a
valid depth map to a challenger that will itself be dark shortly.

The obvious minimal change — keep a dark resident in its slot — was tried and is
wrong: with 30 lit candidates for 16 slots there is always a lit challenger, so
dark residents are evicted anyway and nothing improves. Capacity really is
contested; the question is not *whether* to evict but *how fast*. That wants a
per-slot dark-frame counter (evict only after N dark frames, N tuned against
strobe periods) and a measurement of redrawn maps per frame in both windows
before and after. `ShadowStats::redrawn_maps` already exists but does not cross
the worker seam — `AsyncPresentation` carries `FrameTimings` and would need to
carry this beside it.

Measured end-to-end against the real library, the window costs more but is not
on its own fatal on this machine: UI-thread `draw` 4.6 → 7.2 ms and `GPU` 0.21 →
1.20 ms at 1200x800. The gap between that and "perf goes bad" is unexplained and
is the reason the policy change is specified here rather than guessed at.

### Instruments

Both are `#[ignore]`d and take a library *copy* — opening a library runs
migrations, so neither may be pointed at a live one.

- `compositor::tests::profile_a_real_score_across_a_window` (src-tauri) — eval
  cost, lit count, strobing count and `dimmer_sum` per frame across a window.
- `visualizer_real_score_window` (gpui-agent, `--features pixel`) — the same
  window played through the real renderer, reporting the UI/GPU/present split
  per second of track time.

## 19. Zoom — what it is not, and the instrument that replaces guessing

The zoom-in freeze was reported still live after the `opt-level` fix. Driven
against the user's own library — Club rig, real fixture classes, real haze
defaults, track playing through 0:49 — it does not reproduce, and three
specific suspects are now ruled out rather than merely untested.

**Shadow tenancy does not churn on camera motion.** Priority is
`intensity * radius^2 / distance^2`, so the camera *is* an input to which cones
hold slots, and `1/d^2` is steep close in — the obvious reading is that zooming
reorders the near cones and each reordering redraws a depth map. It is wrong:
`EVICTION_MARGIN` requires a challenger to beat the resident by 25%, which a
dolly never manages at either distance. Pinned by
`gpu::tests::dollying_the_camera_does_not_reshuffle_shadow_tenancy`.

**The cluster grid does not rebuild under zoom.** `cpu_cluster_ms` is non-zero
only on a rebuild, and it reads 0.00 across an entire zoom traversal.

**The zoom range is bounded, and the whole of it is flat.** `dolly` clamps to
`Framing::radius_bounds`, whose near end is proportional to the rig's own
radius — the camera cannot fly into the beams. Forty presses of Zoom In move
1.3% of the pixels and then stop at the clamp. Across that entire permitted
range, at 1000x700, playing: `draw` 3–13 ms with no trend, `GPU` 0.18–1.63 ms
with no trend, `PRES` flat. Whatever the operator is hitting, it is not a cost
that rises with closeness on this rig.

### The hitch recorder

Since the bug will not come to us, the numbers have to come from the machine it
happens on. `HitchRing` in `visualizer.rs` keeps four seconds of `FrameSample`
— present interval, `draw`, the score/build/pick split, CPU encode, GPU total,
cluster rebuild, camera radius, viewport size, lit cone count — one fixed-size
struct copy per frame, no allocation. When a frame reaches the screen more than
`HITCH_MS` (50 ms) after the one before it, the run-up is written through the
existing `append_render_telemetry` command, at most once per `HITCH_COOLDOWN`.
It reuses the log the old React visualizer already wrote, rotation and cap
included, rather than opening a second one.

Always on, because the alternative is asking the operator to notice the bug
twice.

**What it already says.** Provoked at 2200x1400, the captured hitches are
63–137 ms present intervals whose measured phases do not add up to them:
one frame shows `interval 97.7 ms` against `draw 16.1`, `GPU 0.4`,
`cluster 0.0`, score/build/pick under 0.4 combined. Roughly 80 ms is late for a
reason **none of the current numbers name** — it is not evaluation, not frame
assembly, not the hit-test, not encode, not GPU, not clustering.

One caveat, because it is easy to over-read. `draw_ms` is
`PendingFrame::started.elapsed()` — submit to *observed* completion, polled by
the worker loop — so it is a latency, not a cost, and a large value can mean the
worker polled late rather than that anything worked hard. `interval_ms` is
likewise the spacing of frames reaching the screen, which includes the UI thread
simply not asking for one. The honest statement is therefore narrower than
"80 ms of unaccounted work": what is established is that no *measured phase*
grew, and the two numbers that did grow are both latencies whose components are
not yet separated. Separating them is the next instrumentation step —
specifically the span from `request_animation_frame` to the next prepaint, and
the readback/slot wait inside `draw_time`, neither of which anything times.

### Zoom during the heavy window — the compound case

Tested directly, since the two were expected to compound: zoom driven into the
beams *while* `bass_strobe` has all 30 cones lit, at 1200x800, playing. It does
not compound. Across ten zoom bursts, sampled both mid-gesture and settled:
`SHADOWS 0 redrawn` on every reading, `CLUSTER 0.00` on all but one (0.33 ms),
`GPU` steady at 0.4–1.0 ms, `UI` flat at 0.3 ms. The occasional `DRAW` outlier
(44.5 ms once, against `GPU 1.03` and `CPU 0.48` on the same frame) is the
latency-not-cost number described above.

So neither co-P1 reproduces here, and both fail the same way: every phase we
instrument is fine, and what moves is a latency we do not yet decompose.

## 20. The gap, named: it is the UI thread, not the renderer

The two spans nothing measured are now measured, and between them they answer
the question §19 left open.

`FrameSample` gained three fields:

- `ui_frame_gap_ms` — wall time since the stage's previous prepaint, i.e. the UI
  thread's own cadence. Taken at the top of the prepaint, before anything in it
  runs, so it describes the gap and not the frame.
- `request_to_prepaint_ms` — `request_animation_frame` to the stage's prepaint.
  Whatever the UI thread did *before reaching the stage*, which names a stall
  belonging to some other view without needing to instrument that view.
- `queued_ms` — how long the renderer left a submitted frame before starting it.
  Carried on `FrameRequest` and stamped at pickup: `draw_time` starts at submit
  *to the GPU*, so without this a renderer that is backed up and one that is
  slow look identical.

### What they say

Four captured hitches, real library, 2200x1400, playing through 0:49:

| hitch ms | ui gap | req→prepaint | ui gap ÷ interval | queued | draw | gpu |
|---|---|---|---|---|---|---|
| 77.2 | 77.3 | 1.5 | **1.00** | 0.01 | 6.9 | 1.22 |
| 58.5 | 51.1 | 22.5 | **0.87** | 0.01 | 7.3 | 0.20 |
| 113.5 | 28.4 | 1.3 | 0.25 | 3.85 | 20.8 | 1.43 |
| 81.1 | 28.5 | 1.3 | 0.35 | 11.38 | 8.1 | 0.56 |

Across all 722 recorded frames, 144 had an interval over 33 ms. Of those, **81
(56%) had a UI-thread gap accounting for more than 70% of the interval** — the
frame was late because *no frame was produced*. Median GPU on a slow frame:
1.39 ms. Median queue wait: 0.01 ms.

**So the dominant cause is the UI thread not running frames, and it is not the
stage's work** — score, build and pick together stay under 0.5 ms on every one
of these. The first capture is the cleanest statement of it: a 77.2 ms present
interval against a 77.3 ms prepaint gap, with the GPU at 1.22 ms.

The remaining ~44% are renderer-side latency rather than UI: `draw_ms` of 20.8
and 8.1 against GPU of 1.4 and 0.6, with `queued_ms` of 3.85 and 11.38 — the
renderer backed up behind earlier frames, not working hard.

**This redirects the fix out of the renderer.** The app's UI thread is shared,
so an 80 ms stall anywhere on it blocks presentation and reads as "the
visualizer froze". `request_to_prepaint_ms` of 22.5 ms on the second capture is
that in miniature: a fifth of the gap was spent before the walk even reached the
stage. Whoever owns the other per-frame UI work — the chat while a turn streams
is the standing suspect — owns this.

One caveat on precision: `ui_frame_gap_ms` is recorded at the prepaint that
picked up a completed frame, while `interval_ms` belongs to the frame being
presented, so the two are one pipeline stage apart. The correlation is
directional evidence, not a per-frame identity. The direction is not in doubt at
these magnitudes.

### The capture is self-describing

A report carries a `schema` block: units, frame order, the index of the late
frame, the threshold that fired, a one-line meaning for every field, and a
`reading` note giving the UI-gap-versus-interval test above. A capture arriving
from a machine we cannot ask questions of has to be decodable cold.

## 21. The same measurement, paired correctly — and the retraction it forces

§20 concluded that the UI thread was not producing frames. **That conclusion was
wrong, and the error was exactly the one §20 flagged as a caveat and then
reasoned past.** `ui_frame_gap_ms` was recorded at the prepaint that *picked up*
a completed frame, while `interval_ms` belonged to the frame being *presented* —
one pipeline stage apart. Correlating them produced a relationship that was an
artifact of the misalignment.

### The fix

The UI-thread spans are now measured in the prepaint that submits a frame and
carried with it through `SerialPairing`, the mechanism this file already uses to
return a hit-test snapshot with the frame it belongs to. They are not threaded
through the renderer: none of it is the renderer's business, and the pairing was
already the right seam. `SubmittedFrame { pick, spans }` is what a submission
now records.

Added at the same time, to split the "thread went to sleep" bucket:

- `renders_in_gap` — stage renders during the gap. One is healthy; **zero over a
  long gap means nothing asked for a frame** (starvation), which has a different
  fix and a different owner from a thread that was busy.
- `shared_surface` — whether the frame crossed on the zero-copy `IOSurface` or
  the CPU readback fallback, whose copy and buffer map sit *inside* `draw_ms`
  and are invisible to `gpu_total_ms`.

### What it says now

482 frames, 236 of them over 33 ms:

| | median |
|---|---|
| `interval_ms` (slow frames) | 44.1 |
| `ui_frame_gap_ms` | **6.5** |
| `draw_ms` | 20.2 |
| `gpu_total_ms` | 2.12 |
| `queued_ms` | 0.06 |
| `draw_ms − gpu_total_ms` | **18.1** |

- The UI thread is fine. It produces frames every ~6.5 ms — roughly 150 Hz — and
  its gap explains more than half the interval in **4 of 236** slow frames. The
  §20 figure of 56% was the artifact.
- `renders_in_gap` is **1 on every slow frame**. There is no request starvation.
- `shared_surface` is **true on every frame**. The CPU readback path is not in
  use, so its copy is not the explanation either.
- `queued_ms` is ~0. The renderer is not backed up behind earlier frames.
- What is left: **`draw_ms` exceeds `gpu_total_ms` by ~18 ms**, and `draw_ms`
  explains more than half the interval in 128 of 236 slow frames.

### Where that leaves it

`draw_ms` is submit to observed completion. Inside it: CPU encode (measured,
0.5–0.8 ms), GPU pass execution (measured, 2.1 ms), and *waiting*. The worker
polls on a 1 ms `wait_timeout` whenever a frame is in flight, so it is not
polling latency. The remaining candidates are the command buffer waiting to
begin executing — the renderer's device and gpui's compositor share one physical
GPU, and the stage here is 3886x1004 alongside the app's own UI — and the
completion callback's delivery. Both are "the frame sat waiting", not "the frame
worked hard", which is consistent with every phase we measure being small.

Note that `share.rs` names this hazard in its own module docs: sharing removed
the copy *that was also acting as a fence*, and nothing serialises the two
devices. Timestamps bracket pass execution, not the wait to start.

**Do not act on this yet.** It is one machine, in a harness, at a window size
the operator does not use, and the last two confident conclusions in this
document were both overturned by the next measurement. The instrument now
carries every field needed to settle it from a real capture; the operator's own
hitch reports will say whether their `draw_ms − gpu_total_ms` looks like this.

## 22. The live capture — what the operator's own machine says

Six hitch reports, real session, 2026-08-23 17:12:44–17:13:51 UTC, stage
viewport 1526x774. One report every ~10 s, which is the cooldown firing every
time it was allowed to: the session was hitching *continuously*, and six is a
floor on how many hitches there were, not a count.

### The headline

**45 late frames — 3.1% of the frames — hold 50% of the wall clock.** In the
worst capture it is 79%. That is the operator's experience stated as a number:
most of the session is spent inside a handful of frames.

| capture | late frames | wall | lost | lost % | camera |
|---|---|---|---|---|---|
| 0 | 1 | 2.3 s | 0.1 s | 4% | 4.4 → 10.9, **moving** |
| 1 | 1 | 2.1 s | 0.1 s | 6% | 4.4 static |
| 2 | 15 | 5.3 s | 3.1 s | **59%** | 5.4 static |
| 3 | 1 | 2.3 s | 0.1 s | 5% | 4.4 static |
| 4 | 18 | 9.9 s | 7.8 s | **79%** | 4.4 static |
| 5 | 9 | 3.3 s | 1.4 s | **40%** | 4.4 static |

### Every bucket in the decision tree is refuted

On **all 45** late frames, from the operator's machine:

- `ui_frame_gap_ms` = 8.3 ms (median), identical to the healthy median. The UI
  thread never stopped producing frames. Not "UI silent".
- `renders_in_gap` = 1 on every one. Not starvation.
- `request_to_prepaint_ms` = 0.85 ms. Not another view holding the walk.
- `shared_surface` = true. Not the CPU readback.
- `gpu_total_ms` = 1.14 ms. Not GPU work.
- `cluster_ms` > 0 on 19 of 45; `redrawn_shadow_maps` ∈ {0, 1, 4}. Neither is
  doing anything a 171 ms frame could be made of.

Every hypothesis this document has entertained is dead on the operator's own
data. The tree returns **UNATTRIBUTED** for all six late frames.

### And it inverts the zoom story

The one capture whose camera was *moving* — a zoom out from 4.4 to 10.9 — is the
**healthiest of the six**, losing 4%. The three worst (79%, 59%, 40%) all had a
completely static camera. Moving the camera was fine; sitting still, zoomed in
at the near end of `radius_bounds`, was catastrophic. Whatever "it freezes when
I zoom in" is, the freezing does not need the zooming.

### The one signal that moves

`queued_ms` — UI-thread submit to worker pickup, measured entirely on the CPU
before any GPU work:

| | healthy | late | ratio |
|---|---|---|---|
| `queued_ms` | 0.009 ms | 3.82 ms | **~440x** |
| `draw_ms` | 8.2 ms | 17.2 ms | 2.1x |
| `gpu_total_ms` | 0.22 ms | 1.14 ms | 5x |
| `ui_frame_gap_ms` | 8.3 ms | 8.3 ms | 1.0x |

The 440x jump is consistent in all six captures (late 3.1–5.1 ms against healthy
0.008–0.010 ms). In steady state the worker picks a frame up essentially
instantly — it is idle waiting. At a hitch it is not.

### But none of it sums to the interval — and here is the instrument's blind spot

A 345 ms interval with an 8.3 ms prepaint cadence means the UI thread ran ~40
prepaints during the gap, each of which submitted a frame, and **exactly one
frame came back**. The other ~39 are *not in the log*: `HitchRing::record` is
called only on the `Ok(Some(completed))` arm, so a frame that was submitted and
never delivered leaves no trace. The run-up therefore looks like a clean 8.3 ms
sequence with one 345 ms row in it, which is an artifact of what is recorded —
the timeline is not contiguous.

**This failure mode falls exactly into that blind spot**, which is why the tree
returns UNATTRIBUTED: the answer is in the frames the instrument declines to
record.

### Verdict on what to build

**Not a cross-device fence, and not `MTLEvent`.** The GPU-contention candidate
from §21 is not supported by this data: `gpu_total_ms` is 1.14 ms on late
frames, and the metric that moves by two orders of magnitude is measured before
any GPU work happens. Building a fence would address a hazard the operator's
machine does not show.

The next step is to stop the instrument lying by omission, and it is small:

1. Record a sample on the `Ok(None)` arm — frame submitted, nothing delivered —
   so the ~39 invisible frames appear.
2. Carry `SubmitOutcome` (`Queued` vs `Replaced { dropped_serial }`) into the
   sample, which says directly whether submissions are being dropped before
   they render.
3. Carry presentation-slot occupancy at submit, which says whether
   `startable_slot` was returning `None` — the mechanism that would make
   `queued_ms` jump.

Those three turn a 345 ms one-row mystery into ~40 rows that name the mechanism.
**What proves it during a fix:** the same capture showing `queued_ms` back at
0.009 ms and the count of `Ok(None)` prepaints per delivered frame at ~0.

Everything before that is theorising, which this document has now done four
times and been wrong four times.

## 23. The instrument stops lying, and names the mechanism

Four changes, all in the seam this document has been circling:

1. **A sample on every prepaint**, not only on the ones that got a frame back.
   `delivered: false` rows carry everything known before submission and `None`
   for everything measured on the way home. This is the row that was missing.
2. **`replaced_undelivered`** — this submission pushed an older frame out of the
   queue before it ever reached the screen.
3. **A slot census at submit** — `idle`/`rendering`/`ready`/`reserved` plus
   `slot_startable`, read under the same lock as the submission so it describes
   the pipeline the frame actually joined.
4. **`gpu_total_ms`, `cpu_encode_ms`, `cluster_ms` are now `Option`.** Only one
   frame at a time is profiled; reporting the previous frame's number as this
   one's made consecutive rows repeat a value that belonged to neither. `null`
   is the honest answer.

### What the first capture says

498 frames, 271 of them ghosts — **54% of prepaints submitted a frame and got
nothing back**, and none of them existed in the log before this change.

| | delivered | ghost |
|---|---|---|
| rows | 227 | 271 |
| `slot_startable == true` | **227 (100%)** | 118 (44%) |
| `slot_startable == false` | **0** | 153 (56%) |
| `replaced_undelivered` | — | 55 (20%) |

Slot census, `idle/rendering/ready/reserved`:

- delivered: `3/1/0/2` (114), `4/0/0/2` (112) — a free slot, every time.
- ghost, stalled: **`2/2/0/2` (153)** — two slots rendering, two idle *but
  reserved*, nothing startable.

Ghost frames preceding a delivery: median **6–8** before a late one (interval
79–142 ms), median **0–1** before a healthy one (interval 9–14 ms).

### The mechanism

`PRESENTATION_SLOTS` is 4 and `RESERVED` is 2, and the reserved slots read as
*idle but unusable* — they are withheld because their shared surface is still on
screen. **The pipeline therefore runs at an effective depth of two.** When both
usable slots are rendering, `startable_slot` returns `None`, the submission
waits (this is what `queued_ms` was measuring), and every prepaint in that
window becomes a ghost. Six to eight ghosts at an 8.3 ms cadence is 50–70 ms,
which is the hitch.

Delivery requires a startable slot in **100%** of observed deliveries, with zero
exceptions. That is as close to a mechanism as this document has got.

This is the same reservation the zero-copy work introduced deliberately: a
shared surface being displayed must not be drawn into. The question it raises is
not whether to reserve, but whether four slots with two reserved is the right
ratio — and that is a design decision with a memory cost (each slot is a
full-viewport surface, ~4.7 MB at this size) and a correctness constraint, not
something to tune from one capture.

**What still is not explained** is why `draw_ms` sits at 17–27 ms when
`gpu_total_ms` is ~1 ms. Slot exhaustion is the amplifier — two slots cannot
absorb a 27 ms frame — but the 26 ms of non-GPU latency inside `draw_time` is
the §21 mystery, still unnamed. Both halves matter: the depth sets how much
latency the pipeline can hide, and the latency sets how much it has to.

**Acceptance for any fix**, unchanged: a live capture with `queued_ms` back at
~0.009 ms and ghost rows per delivered frame at ~0.

## 24. The last span: the latency is real, and it is not the worker

`draw_time` is now split at the driver's completion callback. `until_signalled`
is submit until the callback fired — the GPU's share, including any wait to
begin executing. `until_noticed` is that callback until the worker's poll
observed it. The stamp is taken *inside* the callback, so the two are measured
where they happen, not where they are read.

236 delivered frames:

| | draw | until_signalled | until_noticed | gpu_total |
|---|---|---|---|---|
| `draw` < 12 ms | 10.8 | 10.8 | **0.00** | 0.7 |
| `draw` >= 12 ms | 18.2 | 18.2 | **0.00** | 1.92 |
| all | 14.7 | 14.7 | **0.00** | 0.74 |

`until_noticed` is 0.00 on **every frame**, including the worst (40.1 ms draw,
40.1 ms signalled, 0.00 noticed). The worker's 1 ms `wait_timeout` is doing
exactly its job. **"The worker noticed late" is dead** — a hypothesis this
document held twice.

So `draw_ms` is `until_signalled` in its entirety: the driver takes 15–40 ms to
report a submission complete while pass-boundary timestamps say the GPU work was
0.7–1.9 ms. The latency is real and it is on the GPU/queue side of the seam.

### Which world that puts us in

The first one. The latency is real rather than an artifact of nobody looking, so
a deeper pipeline is the right absorber and the ratio question does answer
itself — with one caveat worth checking before spending surfaces on it.

**`Queue::on_submitted_work_done` is a queue-wide fence, not a per-frame one.**
It fires when everything submitted so far has finished, so with two slots in
flight, frame N's completion can be gated on frame N+1's execution. That would
inflate `until_signalled` well past a frame's own GPU time, which is exactly the
15 ms-against-1 ms shape observed — and it compounds with depth, because more
slots in flight means more work each callback waits behind.

If that is what is happening, adding slots buys less than the arithmetic
suggests, and a per-frame fence is both cheaper and more correct. Distinguishing
them is one measurement: submit with only one slot in flight and see whether
`until_signalled` collapses toward `gpu_total`. If it does, the fence is the
bug; if it stays at 15 ms, the wait-to-begin is real and depth is the answer.

That measurement should come before the slot-count decision, not after.

## 25. The fence is exonerated — depth is the right lever

The experiment from §24: cap in-flight work to one frame (`LUMA_INFLIGHT_LIMIT`,
env-gated, no-op unset) so `on_submitted_work_done` has nothing behind it to
wait for. Same scene, same window, same track window.

| | delivered | ghosts | `until_signalled` p50 | p95 | `until_noticed` | `gpu_total` | `interval` p50 |
|---|---|---|---|---|---|---|---|
| baseline (up to 4) | 243 | 255 | **14.5** | 25.0 | 0.00 | 0.72 | 25.9 |
| capped to 1 | 99 | 160 | **17.0** | 35.8 | 0.00 | 0.75 | 37.6 |

`until_signalled` did not collapse. It got slightly *worse* — 14.5 to 17.0 at
p50, 25.0 to 35.8 at p95 — while GPU pass time was unchanged at 0.72 vs 0.75 ms.

**The queue-wide fence is not the explanation.** With one frame in flight there
is nothing for a queue-wide callback to be gated on, and the callback still
takes 17 ms to report 0.75 ms of work. The wait to begin executing is real.

### The positive control nobody asked for

Reducing effective depth from two to one made everything worse in the direction
the depth story predicts: **59% fewer frames delivered** (243 to 99) and
**45% worse interval** (25.9 to 37.6 ms p50). Depth demonstrably moves
throughput on this workload, which is the evidence the 4→6 change wanted and did
not have. It is a measured sensitivity, not an extrapolation from arithmetic.

So: `RESERVED` stays 2, the fence stays as it is, and raising
`PRESENTATION_SLOTS` is the supported change — effective depth 2→4 for ~10 MB.

### One loose thread, flagged not asserted

`until_signalled` sits at 14.5–17 ms against 0.75 ms of pass time, and ~16.7 ms
is one refresh period at 60 Hz. That is close enough to be worth one look at
whether completion callbacks are being coalesced to a display cadence, and far
enough (baseline p50 is 14.5, below a full period) that it is not obviously that.
It does not change the verdict — depth absorbs the latency either way — but if
the latency turned out to be display-linked it would cap what any depth can buy,
and that is worth knowing before promising a number.

**Acceptance for the depth change is unchanged**: a live capture with
`queued_ms` back at ~0.009 ms and ghost rows per delivered frame at ~0.

The `LUMA_INFLIGHT_LIMIT` lever has now answered the question it existed for and
should be deleted with the depth change unless it is wanted for the re-measure.

## 26. Shipped: depth 4→6, and the local re-measure

`PRESENTATION_SLOTS` is 6, `RESERVED` stays 2 — usable depth 2→4. The
`LUMA_INFLIGHT_LIMIT` lever has been deleted; it answered its question in §25
and an experiment knob left in the tree is just a way for someone to reproduce
an experiment nobody is running.

Same scene, same window, same track window as the §25 baseline:

| | 4 slots (depth 2) | 6 slots (depth 4) | |
|---|---|---|---|
| delivered frames | 243 | **316** | +30% |
| ghost rows | 255 | 182 | −29% |
| ghosts per delivery | 1.05 | 0.58 | −45% |
| **ghosts with `slot_startable == false`** | **56%** | **5%** | **−91%** |
| interval p50 | 25.9 ms | **15.4 ms** | −41% |
| frames over 50 ms | many | 3 | |
| `queued_ms` p50 | 0.009 ms | 0.009 ms | unchanged |
| `until_signalled` p50 | 14.5 ms | 17.1 ms | unchanged |

### On the acceptance test, honestly

I wrote the acceptance as "`queued_ms` ~0.009 and ghosts-per-delivery ~0". The
first is met. **The second is not, and the criterion was wrong when I wrote it.**

Ghosts per delivery is 0.58, not ~0, and it should not be ~0: a prepaint that
submits while the pipeline is still working on an earlier frame is *normal
pipelining*, not a stall. I did not understand that when I set the target,
because at depth 2 almost every ghost was a stalled one and the two were
indistinguishable in the data.

The metric that actually separates them is the one the slot census added:
a ghost with `slot_startable == false` is a prepaint that *could not* start a
frame, and those went **56% → 5%**. That is the number the acceptance test
should have named, and it is the one to check on the operator's session.

### What did not change, as predicted

`until_signalled` is unmoved (14.5 → 17.1 ms). Depth absorbs latency; it does
not reduce it. The 15-ish milliseconds between submitting a frame and the driver
reporting it done is still unexplained, and still sits near one 60 Hz refresh
period. Three frames over 50 ms survived, so this is a large improvement rather
than a cure — which is what the loose thread in §25 predicted and why no
frame-rate promise should be attached to it.

The operator's session #2 is the real acceptance.

## 27. Session #2 — the mechanism confirmed, and a second disease found

The operator's second capture carries the slot census and the `draw_time` split,
but its census sums to **four** slots on every row: their build predates the
depth change. So this is not the acceptance read — it is a **second independent
baseline**, on a different session, a different window (2172x1123) and a
different camera.

### Everything §22–§24 claimed reproduces

- **188 of 188 stalled ghosts have census `2/2/0/2`.** Two rendering, two idle
  but reserved, nothing startable. Identical signature, different machine state.
- `until_noticed` p50 **0.00**. The worker-noticed-late hypothesis dies again,
  independently.
- `queued_ms` p50 **0.009** on delivered frames.
- **The zoom inversion holds, now 2 for 2.** Report 6 had a moving camera
  (5.7 → 16.5, zooming out) and lost 12% of its wall clock. Report 7 had a
  static camera at 6.6 and lost **78%**. In both sessions, the capture with the
  moving camera is the healthy one.

### But their stall is not the stall this document has been fixing

On the operator's machine a frame is **fast**: `until_signalled` p50 5.6 ms,
p95 14.1 ms, and — across all 270 delivered frames — a **maximum of 25.6 ms**.
Nothing they render is slow.

And yet:

- Stalled runs of **5 to 38 consecutive prepaints**, spanning **60 to 459 ms**,
  with the census pinned at `2/2/0/2` and `ready` at 0 for the entire run.
- The frame that *ends* each stall renders in **3.1–4.5 ms**. The same work,
  immediately after.
- No delivered frame anywhere in the capture carries a stall-length
  `until_signalled`. The frames that occupied those two slots were never
  delivered at all.

So two GPU submissions stop signalling completion for hundreds of milliseconds,
then release together, while identical work takes 3 ms either side of the gap.
That is not a slow frame occupying a slot. That is the GPU work not completing.

### What that means for the depth change

**It will not fix this.** With six slots, four frames get stuck instead of two;
the stall's wall-clock length is set by whatever un-sticks the completion, not by
how many slots are waiting on it. More depth means more frames in flight to lose.

Two different diseases have been in play, and only now can they be told apart:

| | harness (§25–26) | operator (§27) |
|---|---|---|
| per-frame `until_signalled` | 14.5 ms | **5.6 ms** |
| stall length | 50–70 ms | **60–459 ms** |
| stall cause | genuine latency, depth 2 too shallow | completions stop entirely |
| depth 4→6 | **−41% interval, measured** | no reason to expect help |

The depth change is correct and measured for the first. It is shipped, it is not
harmful, and **no one should expect it to fix the operator's lag.**

### The next question, and it is a new one

Why do GPU submissions on that machine intermittently stop completing for 60–460
ms while the same work takes 3 ms either side? The worker is alive and polling
throughout (`until_noticed` 0.00 when the callback finally comes), the content is
static (30 lit cones, camera fixed), and the frames are small. Candidates worth
distinguishing: driver-level preemption by another GPU client, a power or thermal
state transition, surface allocation blocking, or the compositor holding
something the renderer needs. None of these are visible from inside our process
today, which is the honest limit of this instrument.

## 28. Session #3 — the depth change is the regression. Revert it.

Three builds are now on record in one log: session 1 (no census), session 2
(census + split, 4 slots), session 3 (census + split, **6 slots**). Session 3 is
the acceptance read, and it fails.

### The falsification test fired exactly as predicted

§27 predicted that if depth was not the binding constraint the stalled census
would become `2/4/0/2`. It is `2/4/0/2` on 178 of 178 stalled ghosts. Four
usable slots, all rendering, nothing startable — the same stall with more frames
caught in it.

### The outcome metric did not move, across three sessions and two builds

| | S1 (4 slots) | S2 (4 slots) | S3 (**6 slots**) |
|---|---|---|---|
| wall clock lost to late frames | **50%** | **49%** | **49%** |
| interval p50 | 8.3 ms | 8.3 ms | 8.4 ms |
| `until_signalled` p50 | — | 5.6 ms | **10.8 ms** |
| `until_signalled` p95 | — | 12.8 ms | **52.7 ms** |
| `gpu_total` p50 | 0.22 ms | 0.23 ms | 0.25 ms |
| `until_noticed` p50 | — | 0.00 | 0.00 |

Nothing shipped has moved the number the operator experiences. What did move is
per-frame latency, and it moved the wrong way.

### Why: the queue is serial, so depth buys latency, not throughput

`until_signalled` against how many frames were already rendering at submit:

| frames already in flight | S2 (2 usable) | S3 (4 usable) |
|---|---|---|
| 0 | 4.7 ms | 5.6 ms |
| 1 | 18.9 ms | 11.9 ms |
| 2 | — | 20.0 ms |
| 3 | — | **47.0 ms** (p90 66.0) |

Monotonic, and it is the signature of a serial queue: a frame's completion waits
behind everything already submitted. There is no async compute in our
wgpu/Metal path — one queue, serial dispatch — so four frames in flight do not
execute in parallel, they execute in turn. Deepening the pipeline cannot raise
throughput when the GPU is the bottleneck; it only puts more frames in the queue
ahead of yours.

At 2 usable slots the worst case was "one frame ahead of you" (18.9 ms). At 4 it
is "three frames ahead of you" (47.0 ms, p90 66 ms). **That is precisely the
40–60 ms the operator is now reporting.** The depth change caused it.

Confirming the GPU is genuinely the bottleneck on the slow frames: those with
`until_signalled` > 40 ms carry `gpu_total` of **9.4 ms** against an overall p50
of 0.25 ms — real pass work, 37x the median, and not shadows (0 redrawn on
those frames).

### The display-cadence hypothesis is also dead

`until_signalled` is not quantized to the refresh period. Median distance to the
nearest 16.67 ms multiple is 5.47 ms, *worse* than the 4.17 ms a uniform random
distribution would give. The distribution is a smooth heavy tail, not a comb.

### Verdict

**Revert `PRESENTATION_SLOTS` to 4.** It costs 10 MB, does not improve the
outcome, and quadruples p95 per-frame latency on the machine that matters.

The harness measurement that justified it (§26, −41% interval) was taken in a
different regime: there `until_signalled` was 14.5 ms against `gpu_total` of
0.72 ms — latency-bound, and latency is what a pipeline hides. The operator's
machine at their window size is throughput-bound on a serial queue, and nothing
hides that. Both measurements are correct; only one of them describes the user.

The real work is unchanged and is what this document was always about: **reduce
GPU cost at large window sizes.** 9.4 ms of pass time for a 3.2 MP volumetric
frame is the number to attack. Pipeline depth was never going to substitute for
it.

## 29. Reconciliation: the census is sound, and §27's framing was too strong

Two things were queried: whether the build-identification method is trustworthy,
and whether §27's "second disease" reading survives the depth-6 data. The first
holds; the second needs correcting.

### The census format, stated exactly

`occupancy` walks `self.gpu`, which is `[GpuSlot; PRESENTATION_SLOTS]`. So:

- **`idle + rendering + ready` always equals the slot count.** There is no
  hardcoded 4 anywhere in the census path; it reports 6 on a 6-slot build.
- **`reserved` is a separate, *overlapping* count.** A reserved slot is also
  counted in whichever state it is in — it is not a fourth disjoint bucket.
  `2/4/0/2` on a 6-slot build means two idle slots (both of them reserved) and
  four rendering. `3/1/0/2` on a 4-slot build means three idle (two reserved)
  and one rendering.

That is why §23's harness rows summed to 4 and the newest live rows sum to 6:
different builds, same format, `reserved` never part of the sum.

### Verified per report

| reports | time | sum | build |
|---|---|---|---|
| 0–5 | 17:12–17:13 | no census fields | pre-instrument |
| 6–7 | 17:35 | **4** | census + split, 4 slots |
| 8–9 | 17:43 | **6** | census + split, **6 slots** |

The operator is right, and §28 already read it that way. §27's "their build is
pre-depth" was about `live2.log`, whose newest reports *were* the 17:35 pair at
four slots. Both statements are correct about different files; only their
juxtaposition suggested otherwise.

### §27 was too strong, and this is the correction

§27 said the operator's stalls were "completions stop entirely" and that depth
was therefore *irrelevant*. The depth-6 data refutes the second half:

- The extra capacity **is** used — stalls now show four frames rendering, not
  two, and `2/4/0/2` on 178 of 178 stalled ghosts.
- Depth is not irrelevant to *latency*: `until_signalled` rises monotonically
  with how many frames were already in flight (5.6 / 11.9 / 20.0 / 47.0 ms).

The accurate statement is narrower: **depth does not change throughput, because
the GPU is the bottleneck and executes serially.** It changes only how many
frames are queued ahead of yours, which is latency, and which is why the change
made p95 four times worse while leaving the 49% untouched.

### What remains genuinely unexplained

Stall spans still reach 613 ms, which is far more than in-flight count times any
GPU pass time measured. But `gpu_total` is sampled from one profiled frame at a
time, so its distribution is sparse and biased — the frames with
`until_signalled` over 40 ms carry `gpu_total` of 9.4 ms against a p50 of
0.25 ms, and at the larger window its max reached 18 ms. Distributional claims
about GPU cost from this field are weak, and I am not going to build another
theory on it.

**The revert recommendation is unchanged and its reasoning is now stronger:**
depth demonstrably buys latency and not throughput on the machine that matters.

## 30. The heavy frame is the scene pass, not the march

### The repro gap was camera distance, and the field was in the sample all along

Every harness measurement in §22–§29 was taken at camera radius **12.4** — the
opening camera, which frames the whole rig. The operator works at **6.4–6.8**,
roughly half the distance. The volumetric march and the scene pass are both
fill-bound, so for eight sections this document compared a different scene to
the one being complained about.

`camera_radius` was in `FrameSample` the entire time. It was never read.

**Methodology note, and the cheapest lesson of the day:** a field that is
recorded but never plotted is not instrumentation, it is a comment. When a
repro fails, the first move is to diff *every* recorded field between the two
runs — not to reason about which one ought to matter. Three of the findings in
this document (`gpu_volumetric_ms`, `gpu_scene_ms`, `camera_radius`) were
already being measured and simply never crossed a seam or a plot.

The zoom input path, checked for the drift bug it looked like: scroll wheel
(`visualizer.rs:3109`), middle-drag (`:849`) and the toolbar buttons (`:2554`)
all call the same `dolly()`, clamped once by `Framing::radius_bounds()`. One
authority, no second clamp, no bypass. The operator is inside the same fence.

### Reproducing at their distance

Dollied to the near clamp on the real Club rig, playing 46–53 s:

| zoom presses | radius | gpu p50 | gpu max | lost % |
|---|---|---|---|---|
| 0 | 12.4 | 0.61 | 4.43 | 26% |
| 3 | 6.3 | 0.71 | **16.39** | 28% |
| 6 | 5.7 | 0.72 | 5.99 | 33% |
| 10 | 5.7 | 0.71 | 9.33 | 31% |

The median barely moves. The **tail** is what distance buys, and it reproduces
the operator's heavy frames (theirs carried `gpu_total` 9.4 ms, max 18 ms).

### The attribution, on a real sample

The four timestamps we already take bound three disjoint spans, and only two
were ever published. Publishing the other two costs no query slots:

`gpu_scene_ms` (first pass to haze start — scene, plus shadow passes when any
ran) + `gpu_volumetric_ms` + `gpu_composite_ms` = `gpu_total_ms`, exactly.

639 profiled frames over four runs at radius 5.7, 17 of them heavy
(`gpu_total` >= 3 ms):

| | p50 (all) | p90 | max | p50 on heavy | share on heavy |
|---|---|---|---|---|---|
| `gpu_total` | 0.71 | 0.81 | 10.23 | — | — |
| **`gpu_scene`** | 0.65 | 0.70 | 10.18 | **4.19** | **99%** |
| `gpu_volumetric` | 0.04 | 0.05 | 7.63 | 0.02 | 0% |
| `gpu_composite` | 0.02 | 0.02 | 7.58 | 0.01 | 0% |

**Dominant segment on heavy frames: scene 15, volumetric 1, composite 1.**
Shadow redraws on heavy frames: p50 0, max 6 — so `gpu_scene` is the scene pass
itself on almost all of them, not shadow encode.

**The fill-bound-march hypothesis is refuted for the typical heavy frame.** The
single 67%-march frame in the first probe was the outlier, not the rule; with a
real sample the march is 0% of the heavy-frame budget at its median.

### Fix candidates, ranked by what this sample licenses

1. **The clustered-light index — the scene pass's own cost.** The scene pass
   shades every covered fragment against its cluster's light list, so it scales
   with covered pixels times lights per cluster. Moving the camera close makes
   fixtures and beams cover the frame, which is precisely when this bites. This
   is the phase-4 work already in this document (Drobot tiles + Z-bins,
   replacing the CSR whose conservative cone bounds put every light in every
   cluster).
   **Confirm before building:** `ClusterStats::mean_lights_per_cluster` is
   already computed and, for the third time in this investigation, never
   crosses into the telemetry. Publish it and check whether it rises at close
   range on heavy frames. That is a one-field change and it either indicts the
   cluster index or clears it.

2. Nothing else is licensed. March step scaling, opacity early-out and
   coverage-adaptive resolution all target `gpu_volumetric_ms`, which is
   **0.02 ms at the heavy-frame median**. They would optimise 0% of the
   problem. The design space in that direction stays closed until a sample says
   otherwise.

The two-population question from the first probe is resolved: there was only
ever one population once measured properly, and it is the scene pass.

## 31. The cluster index is cleared, and the question changes shape

`ClusterStats` now crosses the seam (`mean_lights_per_cluster`,
`occupied_clusters`, `max_lights_per_cluster`) — the fourth field in this
investigation that was already computed and never read. Two runs each at the
opening distance and at the operator's, real Club rig, playing:

| | radius | frames | heavy | lit p50 (heavy) | mean lights/cluster (heavy) | mean ÷ lit | `gpu_scene` p50 (heavy) |
|---|---|---|---|---|---|---|---|
| zoom 0 | 12.4 | 346 | 14 (4.0%) | 14 | 4.96 | 0.35 | 3.44 ms |
| zoom 6 | 5.7 | 257 | 24 (9.3%) | 6 | 4.27 | 0.71 | 3.17 ms |

**The index is not the culprit.** Mean list length is 4–5 against 6–14 lit
cones. The pathology this document feared — "conservative cone bounds put every
light in every cluster", mean approaching the cone count — is not what the live
rig does. Culling is imperfect (the mean-to-lit ratio does worsen with
proximity, 0.35 → 0.71) but the absolute lists are short, and shading four
lights instead of two does not make a 5x frame.

**Phase 4 (Drobot tiles + Z-bins) is therefore not licensed by this evidence.**
It may still be right on its own merits at larger rigs; it is not the fix for
this complaint, and §30's ranking is superseded on that point.

### What the numbers actually show

Heavy frames cost **the same at both distances** — `gpu_scene` p50 of 3.44 ms
far and 3.17 ms near. What proximity changes is how *often* they happen: 4.0%
of frames far, 9.3% near, a 2.3x increase in rate at identical cost.

So the question is no longer "what scales with proximity". It is:

1. **What makes ~5–9% of frames cost 5x the median** (3.2 ms against a 0.71 ms
   `gpu_scene` p50), when lit-cone count and lights-per-cluster do not explain
   it?
2. **Why does being close double the rate** without changing the cost?

Those are different questions from the one this section set out to answer, and
the honest position is that the evidence has moved again. What is now excluded,
with numbers: the volumetric march (§30), the clustered-light index (here),
shadow encode (§30, p50 0 redraws on heavy frames), the presentation pipeline
(§23–§29), the UI thread (§21), and eval (§18).

The next measurement is a comparison, not another counter: take the heavy
frames and the median frames from the *same* run and diff every field already in
the sample. That is the method that found the camera-radius gap, and it costs
one script rather than another seam.

## 32. The diff found a confound, and §31 has to be withdrawn

The heavy-vs-median diff across every recorded field returned one flag that
dwarfs the rest, exactly the discrete kind rather than a continuous one:

**`lit_cones`: heavy frames 6–14, median-cost frames 0. Every single one.**

**68% of all frames in these runs have zero lit cones.** The score spends most
of its time dark — strobe gaps, envelope troughs — and a frame with nothing lit
costs 0.62 ms against 1.07 ms for a frame with lights on.

So "5–9% of frames cost 5x the median" was never a finding. It said: *the few
frames that have lights on cost more than the many that do not.* That is not a
bug, it is the renderer working. I built a comparison between lit frames and
dark frames and read the difference as a pathology.

### What that does to §31

**§31's second claim is withdrawn.** "Proximity doubles the rate of heavy frames
at identical cost" was an artifact: the two zoom runs differ in setup time (six
button presses), so they begin measuring at different playhead positions and
therefore sample *different track content* with different lit fractions. The
rate difference is content, not camera.

§31's first claim — that the clustered-light index is not the culprit — still
stands on its own numbers (mean list length 4–5 against 6–14 cones), and phase 4
remains unlicensed by this complaint.

### Controlling for content, which is what should have been done first

Lit frames only, bucketed by `lit_cones`, compared across distance:

| lit cones | zoom | n | `gpu_scene` | occupied clusters | mean lights |
|---|---|---|---|---|---|
| 6 | 0 | 8 | 0.57 | 2374 | 2.85 |
| 6 | 6 | 58 | **2.11** | 10008 | 4.27 |
| 14 | 0 | 83 | 0.91 | 2374 | 2.85 |
| 14 | 6 | 29 | **0.50** | 10008 | 4.27 |

The two buckets disagree: at six lit cones proximity costs 3.7x more, at
fourteen it costs *less*. Both samples are of usable size. **I cannot claim a
proximity effect on scene cost from this**, and I am not going to pick the
bucket that agrees with the story.

One real observation falls out: `occupied_clusters` is identical within a zoom
level regardless of how many cones are lit (2374 far, 10008 near). The grid bins
cones by geometry, not by whether they are emitting, so occupancy is a function
of camera and rig alone — it quadruples with proximity because the log-Z slicing
repartitions, not because more light is on screen. Whether that drives shading
cost is exactly what the table above fails to establish.

### Where this leaves it

The harness at these settings produces lit frames costing ~1–2 ms. The operator's
heavy frames carry `gpu_total` of 9.4 ms. **The harness is still not reproducing
the magnitude**, and every conclusion drawn from it about *why* a frame is heavy
is therefore suspect — including the ones in §30 that I was confident about.

The honest next step is not another field. It is to stop measuring proxies and
compare like with like: same content, same playhead, same lit-cone count,
camera as the only variable. That means seeking to a fixed track position rather
than letting setup time choose it — a fix to the instrument, not to the renderer.

## 33. The live capture is complete, and proximity is not the problem

### 1. Every field reaches the live path — there is no fifth seam

Verified empirically against a real capture rather than by reading code: all 33
`FrameSample` fields are present, none missing, none unlisted. The six renderer
timings (`gpu_scene`, `gpu_volumetric`, `gpu_composite`, `gpu_total`,
`cpu_encode`, `cluster`) are non-null on **603 of 798** delivered frames — 76%,
because only one frame at a time is profiled.

That sampling is not biased against the frames we care about: the profile slot
is held until its frame completes, so a *slow* frame occupies it longer and is
therefore **more** likely to be profiled, not less.

So the operator's next rebuild gives per-pass attribution of their real 9.4 ms
frames at zero tooling cost, which is the cheapest next data available.

### 2. `track_time_s` — a better fix than seeking

The confound in §32 was not that the harness starts at an arbitrary playhead. It
is that a hitch report is a *ring dumped whenever a frame ran late*, so it
captures whatever content happened to be playing then, whatever the test's own
measurement window says. Seeking would not have fixed that.

Recording the playhead does. `track_time_s` is now in every sample, so any
analysis can hold content constant and vary one thing — **including analyses of
captures already taken**, and including the operator's, where it also tells us
what part of their track was on screen when they hitched.

### 3. Controlled at last: closer is cheaper

Same one-second playhead bucket, same `lit_cones`, camera the only variable.
Ten matched pairs with n>=3 on both sides:

| track s | lit | `gpu_scene` far (12.4) | `gpu_scene` near (5.7) |
|---|---|---|---|
| 38 | 14 | 1.02 | **0.77** |
| 46 | 14 | 0.68 | **0.56** |
| 47 | 14 | 0.58 | **0.29** |
| 48 | 14 | 0.55 | **0.26** |
| 35–38 | 0 | 0.55–0.56 | 0.65–0.68 |

**With lights on, moving the camera closer is consistently cheaper — roughly
half the cost — not more expensive.** Which is what should have been expected: a
closer camera has a narrower frustum, so fewer cones and less rig are in view to
shade. This also agrees with the round-two note already in this document that
zoomed-in frames are cheaper.

Dark frames go the other way by a small amount (+18%), which is noise against a
0.6 ms baseline.

**The proximity thread is closed.** "It gets slow when I zoom in" is not
reproduced at matched content, and the camera is now excluded alongside the
march, the cluster index, shadow encode, the presentation pipeline, the UI
thread and eval.

### Correction to §32, and the parked suspect

§32 said `occupied_clusters` is "identical within a zoom level". It is not —
controlled, it varies frame to frame within one camera position (2374, 1187,
1298, 0 at the same distance). That claim came from medians over a mixed set and
is withdrawn.

The underlying observation is still worth keeping and is **parked, untested**:
occupancy differs by a factor of several under the log-Z slicing in ways that
are not explained by how many cones are lit. It remains the standing structural
suspect for a proximity cost — to be tested against the operator's next capture,
where the playhead is now recorded, rather than against a harness that has now
produced three invalid comparisons in a row.

### What is left

The harness produces lit frames of 0.25–2.1 ms at every camera distance and
content combination tried. The operator's heavy frames are 9.4 ms. **Nothing in
the harness reproduces that magnitude**, and the honest conclusion is that the
answer is not in this machine. The next data is theirs.

## 34. The verdict: it is not a rendering cost, and it is not playback

The full-schema capture (reports 10–13, `PRESENTATION_SLOTS` correctly back at
4) settles it.

### The worst freezes happen with the transport PAUSED

`track_time_s`, added one section ago, earns itself immediately:

| report | hitch | track_time | camera | lit | delivered / ghosts | can't-start |
|---|---|---|---|---|---|---|
| 10 | **1599 ms** | 6.5 s, frozen | 5.7–16.3 | 7 | 58 / 182 | 179 |
| 11 | **9582 ms** | 6.5 s, frozen | 5.7 static | 7 | **1 / 239** | **239** |
| 12 | 264 ms | 1.1–3.1 s, moving | 7.7–15.1 | 0–7 | 183 / 57 | 31 |
| 13 | 71 ms | 12.0–47.7 s, moving | 5.7 static | 0–14 | 212 / 28 | 23 |

The two catastrophic reports have a **frozen playhead** — the transport is
paused. The two mild ones are actually playing. **The worst freezes are not
during playback at all**, which inverts the framing this investigation has
carried since its first sentence.

### What report 11 actually is

A 9.58-second stall on a **static, paused, 7-cone scene**:

- census `2/2/0/2` on **all 239** ghost rows. Two slots Rendering, never
  changing.
- `ui_frame_gap` 8.3 ms throughout — the UI thread produced ~1150 frames during
  the gap, healthy the entire time.
- 114 of 239 ghosts carry `replaced_undelivered` — work submitted and thrown
  away, continuously.
- `until_noticed` 0.003 ms — the worker saw the completion the instant it came.
- The one frame that did complete: `gpu_total` **2.48 ms**, of which scene 2.45.

So two GPU submissions did not complete for roughly nine and a half seconds, on
a scene whose actual GPU work is two and a half milliseconds, while every part
of our process stayed healthy.

**This is not a rendering cost.** No pass, no resolution, no light count and no
algorithm explains a 3800x gap between work submitted and work completed. Every
renderer-side fix this document has considered would change the 2.48 ms.

(Ring limitation worth noting: 240 frames at 8.3 ms covers 2.3 s, so we see only
the last quarter of the 9.58 s stall. The duration comes from `interval_ms`, not
from counting rows.)

### The conditions delta — same machine, different circumstances

The operator's correction is the key: this is the *same hardware* the harness
runs on. So what separates a 9.6-second stall from a harness that has never
exceeded 70 ms is not the machine, it is the conditions:

- **the harness renders offscreen** (`render_to_image`, no window, no display,
  no compositor consuming the surface); the app presents a real `IOSurface` to
  WindowServer on a real display
- the app paints a full UI around the stage; the harness paints the stage
- ambient load, foreground/background state, display power state

**What the current fields can discriminate:** nothing further. Every in-process
signal is exhausted and all of them read healthy — UI thread, worker, queue
depth, slot census, per-pass GPU, eval, presentation path. The instrument has
done its job by eliminating everything it can see.

**What would discriminate the rest,** in cost order:

1. **Window occlusion and application-active state, one field each.** The stall
   happens while paused — plausibly while the operator has switched away or the
   window is obscured, which is exactly when macOS deprioritises a process's GPU
   work. This is cheap, it is in-process, and it would either explain the
   frozen-playhead correlation or kill it.
2. **Display link / refresh state.** Whether the surface is being consumed at
   all during a stall.
3. **Metal System Trace on their machine during a stall.** The only tool that
   sees the driver side of a submission that does not complete. Everything above
   is us guessing at what Instruments would say outright.

### Standing suspects, resolved

The log-Z occupancy thread parked in §33 is not supported here: report 11 holds
`lit_cones` and camera constant across the entire stall, so occupancy cannot be
what varies. It stays parked but drops below the conditions work.

## 35. `window_active` — the condition field, and what it cost

`FrameSample` now carries `window_active`, read from `Window::is_window_active`
at the top of the prepaint. Thirty-five fields, verified reaching a live
capture.

It is free: gpui already exposes window activity, so no vendor patch was needed.
**Occlusion — visible but covered — is a different state and gpui does not
surface it.** That would need a fourth patch to the vendored snapshot
(`NSWindow.occlusionState`), which is not obviously worth it: the correlation
the captures hand us is with a *paused* transport, and the most likely cause of
a paused transport is an operator who has switched away, which
`is_window_active` already sees. Occlusion only adds the narrower case of a
window that is active but obscured.

### A trap worth naming, because this investigation kept falling into it

**`window_active` is always `false` under the headless test platform**, which
has no OS window to be active. That is a property of the harness, not a finding
about it. A harness capture showing `window_active: false` on every row means
nothing; only a real session's value is informative.

Said in the field's own doc comment and in the capture schema, because the
pattern that produced four withdrawn conclusions today was reading a number
whose provenance was not on the number.

### What this field decides

If the operator's next stall shows `window_active: false`, the disease is a
condition of the session — macOS deprioritising a backgrounded process's GPU
work — and the fix is not in the renderer at any level. If it shows `true`, the
switched-away hypothesis dies and the remaining candidates are display power
state and driver-level scheduling, neither of which is visible from inside the
process, and Metal System Trace becomes the next instrument rather than another
field.

Either way it is decided by one boolean rather than by argument, which is what
this instrument has been for.

## 36. Metal System Trace — the verified runbook, and the pre-registered analysis

Switched-away is dead by testimony: the operator is watching the lights, not
clicking away. It lags zoomed in with spotlights active, "when a lot of the
screen is volume". The paused-playhead correlation re-reads as *studying the
beams close-up while paused, lights still lit* — paused is not idle and not
backgrounded. `window_active` will formalise it in their next capture; treat it
as settled.

So the next instrument is outside the process. Everything below was **run on
this machine**, not written from memory.

### Verified facts

- `xctrace version 26.0`; template name is exactly **`Metal System Trace`**.
- **No sudo, and `--no-prompt` suppresses the privacy dialog** — a 3 s
  all-processes recording completed clean.
- The target must already be running for `--attach`; `--all-processes` needs
  nothing started and is what we want, because the question includes *other* GPU
  clients.
- Size: ~42 MB for 3 s all-processes. `--window` bounds it — a 12 s recording
  keeping the last 4 s came to 38 MB, so budget roughly 35 MB fixed plus
  ~4 MB per retained second.
- Export is **fully headless**: `--toc` then `--xpath`. No Instruments GUI at
  any point.

### The operator's command

```sh
xcrun xctrace record \
  --template 'Metal System Trace' \
  --all-processes \
  --window 30s \
  --no-prompt \
  --output ~/Desktop/luma-freeze.trace
```

Start it, reproduce the freeze, wait about five seconds after it clears, then
Ctrl-C. `--window 30s` keeps only the last thirty seconds, so the freeze is
guaranteed to be in the file however long they had to hunt for it, and the file
stays around 150 MB.

`--all-processes` rather than `--attach luma-app` deliberately: attaching sees
only our submissions, and half the pre-registered questions are about who else
was using the GPU.

Then note the wall-clock time of the freeze. Our hitch report carries a `ts`
stamped *after* the stall clears, so the two align.

### Reading it, without the GUI

```sh
xcrun xctrace export --input luma-freeze.trace --toc > toc.xml
xcrun xctrace export --input luma-freeze.trace \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="SCHEMA"]' > SCHEMA.xml
```

### Pre-registered analysis — the question, and the table that answers it

| question | schema |
|---|---|
| our command buffers during a stall: created, scheduled, executing, or none of them? | `metal-application-command-buffer-submissions` (Creation, Duration, Event Type per buffer) |
| are we blocked waiting on the compositor? | `ca-client-buffer-wait-interval`, `ca-client-present-request`, `ca-client-presented-handler` |
| is the surface being consumed at all? | `displayed-surfaces-interval`, `display-surface-swap`, `display-surface-queue` |
| display refresh cadence through the stall | `display-vsyncs-interval` |
| other GPU clients (WindowServer, browser) in the same window | `process-info` plus per-process attribution in the GPU tables |
| GPU power-state transition during the stall | `gpu-performance-state-intervals`, `gpu-performance-device-state-intervals` |
| thermal throttling | `device-thermal-state-intervals` |
| **shader or pipeline compilation mid-session** | `graphics-compiler-activity-intervals` |
| Instruments' own hang detection | `potential-hangs`, `hang-risks` |

The last three are candidates the schema list surfaced that this investigation
had not considered. `graphics-compiler-activity-intervals` is the interesting
one: a pipeline recompiled at runtime can block for seconds, it would be
invisible to every field we have, and it fits a stall whose GPU work is 2.5 ms.

Registering these *before* seeing the data, because the failure mode of this
investigation has been fitting a story to whichever number moved.

## 37. REPRODUCED — in a real window, on this machine

The offscreen harness could never do it. The real app does, on the same
hardware, within a minute of launch.

### The auto-repro

`LUMA_AUTOREPRO=<track title substring>` in `Luma::auto_repro` (lib.rs), with
`LUMA_AUTOREPRO_ZOOM` (dolly steps, default 8) and `LUMA_AUTOREPRO_SEEK`
(track seconds, default 50). It polls until the restored venue's sidebar has the
track, opens it, waits for the stage to exist, dollies to the near clamp, seeks
into lit material and starts the transport.

An instrument, env-gated, documented as such. `main` still takes no flags and
every screen is still reached by pressing something; this exists because the bug
only appears in a configuration the harness cannot produce, and reaching it by
hand every time is how a measurement goes unrepeated.

Two supporting accessors: `Tracks::find_titled` and `Visualizer::dolly_in`,
which repeats the operator's own gesture rather than computing a target radius —
only the camera knows where its near bound is.

### What it produced: four hitches in sixty seconds

| ts | hitch | viewport | delivered / ghosts | can't-start | census | `window_active` |
|---|---|---|---|---|---|---|
| 19:02:34 | 53.6 ms | 1526x774 | 217 / 23 | 17 | `2/2/0/2` | **true** |
| 19:02:44 | 62.0 ms | 1526x774 | 180 / 60 | 49 | `2/2/0/2` | **true** |
| 19:02:56 | 196.1 ms | 2146x1310 | 199 / 41 | 33 | `2/2/0/2` | **true** |
| 19:03:07 | **1171.4 ms** | 2146x1310 | **21 / 219** | **219** | `2/2/0/2` | **true** |

Camera at 5.7 (the near clamp) throughout, transport playing, lit cones 0–30.

The 1171 ms frame: `uiGap` 8.9 ms, `queued` 7.61, `draw` 22.8, `until_signalled`
22.79, `until_noticed` 0.0012, `gpu_total` **5.21**, `gpu_scene` 5.14.

### What this settles

1. **`window_active` is `true` on every one.** Switched-away is dead by
   measurement as well as by testimony. The operator was watching.
2. **The windowed-presentation delta is real and is the reproduction key.** Same
   machine, same rig, same score, same camera. The harness at a *larger*
   viewport (3886x1004, 3.9 MP) never exceeded 70 ms; the window at 2146x1310
   (2.8 MP) reached 1171 ms. It is not pixels. It is the window.
3. **The `2/2/0/2` signature transfers exactly.** Every stalled ghost, in every
   report, in both the operator's captures and this one.
4. The stall grew when the viewport grew (53/62 ms at 1526x774, then 196/1171 ms
   at 2146x1310) — but the harness rules out pixel count as sufficient on its
   own, so size is an aggravator inside the windowed path, not the cause.

### Runbook correction — §36's size estimate was badly wrong

§36 estimated ~14 MB/s from an idle-machine measurement. **Under load the real
figure is ~110 MB/s**: a 40-second all-processes capture with the app rendering
produced **4.5 GB**.

Worse, that trace **failed to export**: `xctrace export --toc` returned
`Document Missing Template Error`. So the largest capture is also the one we
cannot read.

Revised guidance, unverified at the new size and flagged as such: keep captures
short (10–15 s) and consider `--attach luma-app` over `--all-processes` despite
losing other-client attribution, then widen only if the narrow one exports
cleanly. **Do not hand §36's numbers to anyone until a capture has been
exported end to end.**

## 38. The trace: it is not Metal, and it is not the compiler

Reproduced, traced, exported, analysed. A 12-second `--attach luma-app` capture
(300 MB, exports cleanly — unlike the 4.5 GB all-processes one) containing
**five hitch reports**.

### Both remaining candidates are refuted

**Shader compilation: zero rows.** `graphics-compiler-activity-intervals` exists
in the trace and is **empty** across the whole window. Nothing compiled during
five stalls. The candidate fitted every stubborn fact and is simply not what
happens — which is why it was pre-registered.

**Metal execution: nothing exceeds 2 ms.**

| | n | p50 | p95 | max |
|---|---|---|---|---|
| command buffer submissions | 8644 | 0.288 ms | 1.17 ms | **2.0 ms** |
| CA client buffer waits | 1373 | 0.71 ms | 5.48 ms | 25.3 ms |

Not one command buffer took longer than **two milliseconds** in a window
containing hitches of 50–1171 ms. The compositor waits are one per frame,
sub-millisecond at the median, and total 2.32 s of 12 — normal per-frame
CoreAnimation behaviour, not a stall.

### What that leaves, and it is a contradiction worth stating precisely

Our census says two slots sat `Rendering` for the entire stall. Metal says no
command buffer ran longer than 2 ms. Both cannot describe the same work.

The resolution is that **a slot marked `Rendering` does not imply work is on the
GPU.** The slot is claimed at `begin_latest()`, and `submit_live` then encodes
and submits. If the worker blocks anywhere in that span — before the encode that
`cpu_encode_ms` brackets — the slot reads `Rendering`, the frame never reaches
Metal, and the trace has nothing to show because nothing was submitted.

So the stall is **on the CPU side of the renderer worker, between claiming a
slot and handing work to Metal** — the one span in this entire investigation
that is still unmeasured. `cpu_encode_ms` starts too late to see it.

The standing suspect within that span is surface acquisition: `RESERVED` slots
are withheld because the compositor is still reading their `IOSurface`, and
`share.rs` notes that sharing removed the copy that was also acting as a fence.
A worker blocked acquiring or writing a surface the compositor has not released
would produce exactly this signature — `Rendering` slots, no Metal work, healthy
UI thread, and a stall that only exists when there is a real compositor.

**That is a hypothesis, not a finding.** The measurement that settles it is a
timestamp on entry to `submit_live` and another immediately before
`queue.submit`, carried on the frame like every other span. One field-pair, the
same shape as the four that have already worked.

### Runbook, now verified end to end

`--attach <pid> --time-limit 12s` produced 300 MB and exported cleanly.
`--all-processes --time-limit 40s` produced 4.5 GB and failed to export
(`Document Missing Template Error`). Attach, keep it short.

## 39. The span was already measured — and three corrections

§38's closing sentence asks for a field-pair around
`begin_latest()` → `queue.submit`. Building it found that the bracket already
existed.

`submit_readback` takes `let started = Instant::now()` on its third line and
computes `cpu_encode_submit = started.elapsed()` on the line immediately after
`queue.submit`. That is exactly the span §38 calls "the one span in this entire
investigation that is still unmeasured."

**The defect is where the number was stored, not that it was missing.**
`cpu_encode_submit` lived inside `PendingProfile`, which exists only on the one
frame per cycle that carries GPU timestamps. So `cpu_encode_ms` is `null` on
every unprofiled frame — and it was `null` on the late frame of every hitch
report anyone pulled. The measurement was structurally absent from precisely the
frames that stall, while sitting in the schema looking populated.

That is worth naming as a class: **a field that is `Option` for a reason
unrelated to the question being asked will be `None` exactly when it matters.**
`cpu_encode_ms` is optional because *GPU timestamps* are optional. Nothing about
a CPU wall bracket needs the adapter's cooperation, and tying the two together
is what hid a stall for four sections.

### What replaced it

CPU spans now live on `PendingFrame` as `CpuSpans`, always present, never
`Option`, split into five disjoint phases that sum to the total: `prepare`,
`clusters`, `upload`, `targets`, `encode`. `targets` is its own phase because it
is where the shared `IOSurface` is acquired — §38's standing suspect gets a
number rather than an argument.

A test pins the sum: the phases must exhaust the span to within 50 µs. Without
it, a phase added to the encode path and not to `CpuSpans` would lose time
silently while still printing plausible numbers.

### The `ui_frame_gap` beat — new, and unexplained

§34 records the UI thread at a flat 8.3 ms through a 9.6 s stall. That is not
what the shorter stalls do. Through a 233 ms hitch, `ui_frame_gap_ms` reads:

```
4.8  12.0  16.4  4.8  7.3  21.8  4.8  11.7  24.2  4.9  20.7  20.9  5.4  19.5  25.7
```

A cheap frame alternating with a long one, for the whole stall — not a flat
cadence and not a uniform slowdown. A beat pattern like that is what a thread
does when every other pass blocks on something another thread holds. The
candidate is a lock shared between prepaint and the worker; the field-pair plus
a matching bracket on the UI-thread side of that lock would name it. Recorded
because nobody has looked at it, not because it is understood.

### Three retractions

**Windowed framerate.** A capture read as 26 fps and was briefly framed as
"continuous throttle with a bad tail". Wrong: `interval_ms` p50 is 8.3 ms
across every windowed session, and the offscreen harness measures 26.8 ms — the
window is ~3x *faster* than the harness. The 26 fps came from a capture with two
`luma-app` instances competing. The hunt stays "healthy 120 fps, rare severe
tail", which is also what the operator describes.

**GPU power state.** A capture showed performance state `Minimum` for 94% of its
span, `is-induced: Yes`, and it was reported as a candidate mechanism. The same
capture delivered 118 fps. A GPU sustaining 118 fps is not being braked;
`Minimum` there is headroom, not throttling. It is a baseline, interesting only
if a stall-containing capture shows something different. (`powermode 2` = High
Power, thermal Nominal, on AC — the obvious explanations were checked and are
all dead.)

**`window_active` cannot mean "the operator is watching".** §35 leans on it.
But the display *powering down* produces stalls of the same magnitude and the
same signature — 164.7 ms and 1031.1 ms, playhead pinned, `window_active: true`
on both. So the field distinguishes "window not formally inactive" and nothing
more. For unattended runs, display-state transitions belong in the benign-hitch
filter alongside `window_active: false`.

### Runbook, corrected again: stop the recorder from the stall

§36 recorded the hunt; §37 said keep it short. Both tune the wrong dial. Record
a **rolling window** and let the stall stop it:

```sh
xcrun xctrace record --template 'Metal System Trace' --all-processes \
  --window 12s --time-limit 300s --no-prompt --output stall.trace
```

The app writes its hitch report when a stall *clears*, so the retained window is
exactly the span containing the stall. A watcher polls the telemetry log and
SIGINTs the recorder on the first report over threshold. Measured:

| capture | size | export |
|---|---|---|
| full template, 30 s attach | 3.7 GB, 6.5 min to finalize | **fails** |
| reduced instrument set, 30 s | 3.5 GB | **fails** |
| windowed + triggered, all-processes | **86 MB, 7 s wall** | clean |

Reducing the instrument set buys nothing — `Metal Application` is the firehose,
not `Metal GPU Counters`.

Two traps worth carrying forward. **`--attach` cannot answer four of §36's nine
questions**: `display-vsyncs-interval`, `displayed-surfaces-interval` and
`display-surface-swap` have zero rows in an attached capture because those
events belong to WindowServer. And **the app truncates
`render-telemetry.log` on launch**, so the report that triggered a capture is
destroyed by the next run — snapshot it beside the trace.

### Two things that are not the disease

`graphics-compiler-activity-intervals` has **zero rows** across ~15 s of live
windowed rendering in two independent captures. Runtime pipeline-variant
compilation was the pre-registered favourite and there is no evidence for it.

A capture taken with the session **locked** contains zero command buffers — the
app does no GPU work at all behind a lock screen. Useful as a check that a null
result is the machine's state rather than the repro being shy.

## 40. Found it: the worker was destroying finished frames

The disease is `Slots::startable_slot`, in `viewport.rs`, and it has nothing to
do with Metal, the compositor, shader compilation, GPU clocks, surface
acquisition, or presentation depth.

### The measurement that named it

§38 inferred that a slot reading `Rendering` with no Metal work meant the worker
had blocked before `queue.submit`. The submit-span brackets (§39) measured that
directly and **refuted it**: `submit_total_ms` peaks at 5.43 ms across ~1200
frames spanning five hitches, and `submit_targets_ms` — surface acquisition,
§38's standing suspect — reads 0.000–0.005 ms. Nothing blocks before submit.

That left one thing nobody had counted: what the worker *finishes*. `complete`
drops any result at or below `last_presented`, so the frames that stall are
exactly the ones whose timings are discarded, and no sample ever carried them.
A counter incremented on the retire path, before the keep-or-discard decision,
closed that hole. Through a 163 ms freeze:

```
idx  int    wFin  dFin  wSig   del census st
221    0.0   8680   +0  11.91   0  2202 0
222    0.0   8681   +1  13.22   0  2202 0
...
238    0.0   8695   +1  19.23   0  2202 0
239  163.0   8697   +2  13.31   1  3102 1
```

**The worker completed fifteen frames during the freeze**, signalling healthily
at 19–21 ms throughout, and not one of them reached the screen. The GPU was
never stalled. The frames were being destroyed.

### The mechanism

`startable_slot` prefers an idle slot and falls back to recycling "the oldest
completed frame the UI has not consumed". With `PRESENTATION_SLOTS` 4 and
`RESERVED` 2 there are **two usable slots**, so when one is `Rendering` and the
other has just completed, the oldest recyclable frame *is* the newest one — the
frame the next `take_latest` was about to present. `begin_latest` overwrites it
and the slot returns to `Rendering`.

The UI queues a descriptor every frame, so the worker almost always has work
waiting and almost always wins that race. The result is a loop that sustains
itself for as long as the phasing holds: worker finishes a frame, immediately
recycles it for a newer one, UI looks and finds nothing ready, submits again.

It also explains why the bug was invisible. The recycled slot reads `Rendering`,
so the census never shows a `Ready` frame; `slots_ready` is 0 on every stalled
row in every capture in this document. Every field that describes slots was
describing a slot that had just been robbed.

### Why only in a window

The harness drives submit and take from one thread in lockstep, so the worker
never gets a turn between a completion and the take. A real window has the UI
thread on its own vsync cadence, racing the worker — which is the
harness-versus-window delta this investigation chased for six sections, and it
was never about the compositor at all.

### The fix

The newest completed frame is never recyclable. Recycling still exists so a ring
full of stale completions cannot deadlock the worker; it simply may not take the
one frame the UI is entitled to. Two tests pin it: the newest frame survives a
worker with work queued and no idle slot, and an *older* completed frame is
still recyclable so the anti-deadlock path keeps working.

One existing test changed with it. `the_startable_predicate_agrees_with_actually_starting`
asserted that "one completion frees exactly one slot" — the old policy, and the
bug. Its real invariant, that `can_begin` and `begin_latest` always agree, is
preserved and still asserted.

### Verification — A/B, same binary, one filter clause apart

Gated on the stage being confirmed lit, because the auto-repro can land with the
transport past the end of the track where the stage is black, the frame costs
3.5 ms instead of 16.5, and nothing can hitch. An ungated run reports zero and
means nothing by it — which it did, once, before the gate existed.

| build | hitches | over | worst | p50 |
|---|---|---|---|---|
| recycling unrestricted (the bug) | **15** | 300 s lit | 233.2 ms | 95.6 ms |
| newest frame protected (the fix) | **0** | 300 s lit | — | — |
| the fix, repeated | **0** | 300 s lit | — | — |

Three runs each, comparable lit fractions. `cargo test -p luma-render` 52/52,
`cargo test -p luma-app` green.

### What this retires

Dead as explanations, each by measurement rather than by argument: runtime
shader compilation (zero compiler intervals), Metal execution (max 2.0 ms
command buffers), compositor waits (ordinary vsync pacing), GPU power state
(`Minimum` while sustaining 118 fps), surface acquisition (microseconds), and
presentation depth (§26–§29's slot-count tuning was tuning around this bug).

### Addendum: the shadow-redraw correlate, checked and withdrawn

A `redrawn_shadow_maps == 16` reading on the late frame of three hitches looked
like a signature, with an appealing mechanism behind it: if the shadow passes
encoded outside the GPU timestamp brackets, a full-cap redraw storm would be
real work invisible to `gpu_total_ms` and landing squarely in
`until_signalled`. Both halves are false.

**The brackets cover the shadow passes.** Timestamp 0 is claimed by whichever
pass encodes first, and the order is `fixture-shadow` → `shadow-cascade` →
`depth-prepass` (`claim_start_timestamp`, gpu.rs). On a frame that redraws maps,
timestamp 0 is written at the start of the first shadow pass, so those passes
are inside `gpu_scene_ms` and `gpu_total_ms` — as that field's own doc comment
already said. There is no second encoder or command buffer to hide in either:
the frame is one encoder and one `queue.submit`.

**The correlate does not survive more samples.** Across the fifteen pre-fix
hitches, `redrawn_shadow_maps` on the late frame reads
`[16, 0, 0, 6, 8, 0, 0, 8, 0, 8, 0, 0, 8, 0, 0]` — nine of fifteen are zero,
against a base rate of 11.9% of all ring rows carrying any redraw. And the cost
is small and plainly visible where it should be: `gpu_scene_ms` p50 is 0.51 ms
with redraws against 0.17 ms without.

Three samples suggested a signature; fifteen make it a mild enrichment. The
plausible residual role is as a *trigger* — a slightly dearer frame widens the
window §40's recycling race was winning — which needs no mechanism of its own.

### Addendum: two latent races in the same machinery

Adversarial review of the §40 fix confirmed it sound and surfaced two unfenced
hazards next to it. Both are write-vs-read across the two Metal queues, and both
only bite when the GPU backs up past a frame period — which is to say, during
exactly the hitch anyone would be debugging.

**The reservation was one short of the drawable count.** The rule is
`RESERVED >= drawables + 1`, and the extra one is not slack. `next_drawable` is
the only blocking point and it runs in *paint*; `take_latest`, which releases
`S(n - RESERVED)` back to the worker, runs in *prepaint*. So the guarantee in
force when a surface is released is the *previous* frame's acquire — at
prepaint(n), `CB(n-1-D)` has been displayed and `{CB(n-D) ..= CB(n-1)}` may
still be sampling. Safety needs `n - RESERVED < n - D`.

Upstream's `maximum_drawable_count(3)` against `RESERVED = 2` was two short.
Lowering the count to 2 shrinks the exposed window but leaves it one short, so
the constants are now `PRESENTATION_SLOTS = 5`, `RESERVED = 3`, `drawables = 2`.
**Usable depth is unchanged at two** — 5-3 as against 4-2 — so this does not
reopen §26–§29, which was about usable depth going from two to four. The cost is
one extra `IOSurface`.

The coupling itself is the real smell: a counting argument two files must agree
on, one of them vendored, is exactly what a dependency refresh silently
falsifies. Retaining the `CVPixelBuffer` in the window command buffer's
completed handler and releasing the slot from that callback would replace it
with a fence and delete the coupling. Recorded as the standing fix, not done.

**`fail_in_flight` dropped the failure it exists to deliver.** It set surplus
`Rendering` slots straight to `Idle` — "nothing was ever drawn here", hence
immediately startable, while the dead renderer may still have writes
outstanding. Worse, the one slot it did route through `complete` had its result
dropped whenever that slot's serial sat at or below `last_presented`, and
out-of-order completion makes that state ordinary rather than exotic: begin 21,
begin 22, complete and present 22, and slot 21 is still rendering when the
worker dies. Then *nothing* reached the caller — the silent freeze
`supervised_worker` documents itself as existing to prevent.

Fixed with `force_ready`, which makes a result available whatever the
presentation boundary says. Kept separate from `complete` rather than teaching
`complete` to tell a picture from a failure: `Slots` is generic over what a
frame is, and `fail_in_flight` is the only caller that knows a result is an
error. The staleness rule is about *pictures* — presenting a stale one runs time
backwards — and a failure is about the renderer, not its frame.

A correction to the first write-up of this: the stated rationale there, that
electing one slot to carry the message could see it outvoted by a higher-serial
sibling, is wrong. `take_latest` only ever considers `Ready` slots, and the
non-elected slots were `Idle`, so the elected slot was always the only candidate.
The bug was the high-water drop, not an election.

**The surviving picture, closed.** If a slot already held a finished `Ok` with a
serial above every failing slot's, `take_latest` returned that picture and
cleared the errors with it, so the caller still never learned the renderer had
died. An exhaustive search — every slot shape over
{Idle, Rendering, Ready(Ok), Ready(Err)} containing at least one Rendering,
crossed with every serial ordering and every position of `last_presented`,
122,880 censuses — found the minimal witness needs no help from the staleness
rule at all: one surviving good frame with a higher serial, `last_presented` 0.
It is the plain case of a worker dying with a frame already in hand.

`fail_in_flight` now clears surviving successes to `Idle` in a second pass, but
only once it has installed a failure — with nothing in flight there is no serial
to carry a message on, and clearing the last good frame would trade a misleading
picture for a blank one. This is the deliberate exception to "no slot goes
straight to `Idle`", and it is safe on both counts that make direct `Idle`
dangerous elsewhere: the GPU work has completed so nothing is writing the
surface, and the frame was never presented so nothing is displaying it.

The alternative — preferring failures in `take_latest` — was rejected on two
grounds. It changes steady-state semantics, because `poll_live`'s error arm
retires errors during ordinary operation, and suppressing a newer good frame
behind a transient error is a lie about a renderer that is fine. And it walks
`last_presented` backwards, trading a swallowed error for a weakened staleness
boundary. What separates the two cases is "is the worker dead", and only
`fail_in_flight` knows that.

**The boundary now only advances.** `force_ready` admits a failure below
`last_presented` on purpose, but `take_latest` assigned the taken serial
unconditionally, so taking one lowered the mark that `complete` reads to reject
stale completions — silently falsifying that field's own doc ("greatest serial
handed to the presentation caller"). Not exploitable today, since
`fail_in_flight` leaves nothing rendering that could complete beneath the
lowered mark, but the next caller of `force_ready` would have inherited it.
`take_latest` now takes the max.

The vendored snapshot's local edits are now inventoried by
`git diff -- gpui/vendor/` rather than by a marker convention. `gpui/vendor/`
has exactly one recorded commit, so that diff is complete by construction; the
marker convention that preceded it found three of six edits, and the three it
missed included `Window::update_image`, without which the two it found are dead
code.

**The give-up path, which reported nothing at all.** Clearing surviving
pictures only once a failure is installed is right — with nothing in flight the
restart usually succeeds, and announcing a failure there would fabricate an
error over a good frame from a renderer that came back. But it leaves
`fail_in_flight` with one reporting mechanism, silent when nothing was in
flight, and on the *last* attempt that silence is the whole disease:
`supervised_worker` returns, the thread ends, `take_latest` returns `None` for
ever and the stage paints its last good frame — verbatim what that function's
doc says it exists to prevent.

The two conditions correlate the wrong way. A worker that dies on entry dies
before it starts anything, so the permanently-broken renderer is the one least
likely to have a frame in flight to carry the news, and the give-up path only
runs after four consecutive failures — when the message matters most.

Terminal failure is now its own operation. `Slots::announce` mints a serial
above every serial in play and installs the error into a free slot, so
`take_latest` cannot prefer a stale picture over it. *Every* serial, not
`last_presented + 1` — the case that proves the difference has no surviving
picture in it at all: `fail_in_flight` runs first and turns each in-flight slot
into a `Ready` failure, so `last_presented + 1` loses to a message installed one
line earlier. That was found by enumerating the ceiling rather than arguing it. Deliberately not folded
into `fail_in_flight`, whose contract is "fail what was in flight": teaching it
which attempt this is would put a policy it has no business knowing right next
to the guard that keeps it quiet.

The branch was also the least testable in the file, because `render_worker`
acquires a GPU. `supervise` now takes the worker as a parameter, and a scripted
worker that panics on demand pins the restart count, the terminal report, that a
recovered worker leaves nothing behind, and that a stopped viewport is told
nothing. The most consequential branch here had no coverage at all until now.

Re-verified after all of it: 0 hitches over 300 s of confirmed-lit playback,
three runs, with the stage checked lit and playing by screenshot.
`cargo test -p luma-render` 60/60 — including three exhaustive property tests,
over reachable censuses, the death path and the give-up path — and
`cargo test -p luma-app` green.
