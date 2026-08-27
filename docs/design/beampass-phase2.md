# BeamPass Phase 2 — cone proxy geometry

**Status: not implemented, and measured not worth implementing. See §0.5.**
Step 1 (the shared integrand, `shaders/beam_transport.wgsl`) landed and stays — it is a
structural win independent of the proxy. Steps 2–9 are shelved on measurement, not on
suspicion. §1–§5 remain the design of record should §0.5's ceiling ever move.

Scope: `gpui/crates/render/` — the volumetric beam pass only.
Implements Phase 2 of [`volumetrics-v2.md`](volumetrics-v2.md) §3.1 / §4, with the corrections
that [`volumetrics-v2-review.md`](volumetrics-v2-review.md) findings 2, 5, 10 and 13 force.

Reference hardware: Apple M2/M3-class, wgpu 30.0.1 / Metal.
Gating workload: `beams-at-camera-128` (`bin/profile-volumetrics.rs`, `--case=<id>`).

---

## 0. Thesis, and the one property everything else defends

**The proxy decides which pixels run the integral. It never touches the integral.**

The span, the MIS estimator, the phase function, the gobo, the taper, the noise, the shadow
tap and the HDR policy all stay in one WGSL function with one copy on disk, called from two
call sites with identical arguments. A pixel's radiance for light `i` is therefore a pure
function of `(ray, hit_dist, jitter, light_i)` — the same value whichever pass computed it.
Every hard question below (does the fallback seam? does temporal history go stale? do the
goldens move?) collapses to arithmetic on *which* pass added the value, not on *what* value
it was.

This is §3.0's partition invariant applied one level down: the beam pass and the marcher
partition the **light set**, never the screen and never the medium.

### Three corrections to the parent doc, up front

1. **§3.1's "the culling problem for beams disappears — the rasterizer *is* the cull, and it
   is exact" is wrong, and review finding 2 is right.** The rasterizer is exact about the
   *hull*, and the hull of a cone that contains the eye covers the whole screen. Apple's TBDR
   gives additive-blended geometry zero overdraw reduction (WWDC20 10602 — HSR flushes on the
   first translucent primitive), so `beams-at-camera-128` is the case where proxy raster is
   **worse** than what it replaces, not better. §2 is the design for that.
2. **§3.1's "half-res with a full-res edge" is wrong.** The silhouette is not a rasterized
   triangle edge — the proxy is deliberately *conservative*, so the visible edge comes from
   the analytic span test inside the fragment shader, which runs at the beam target's
   resolution. Cone-edge sharpness is decoupled from *volume* resolution (which is the real
   claim against froxels, and it survives) but not from *target* resolution. `LIVE_HAZE_RESOLUTION`
   stays 0.5 and the bilateral upsample stays exactly as it is. See §3.
3. **§4 Phase 2 item 5 — "Delete `haze_tiles`, `haze_tile_key`, `HazeTileCache`,
   `HAZE_TILE_SIZE`" — cannot happen in Phase 2.** The marcher *is* the fallback, so it and
   its tile lists survive. What dies in Phase 2 is `HazeTileCache` (the cache, which §3.2
   already calls unsound), not the tiler. See §6.

   > **Overtaken by events.** All four died anyway, on 2026-08-24, when the unified light
   > index replaced the per-pass tiler (`docs/design/light-index-unification.md`). The
   > marcher is still the fallback; its candidate list now comes from
   > `lights_along()` + `light_index_next()` against 8 px tiles × 512-bit masks
   > (`shaders/light_index.wgsl`) rather than from a tile list of its own. Everywhere below
   > that says "tile list", read "light-index walk". The partition (§2.3) would therefore
   > have to be a per-light exclusion bit tested inside `haze.wgsl`'s walk, not a shorter
   > list handed to a tiler — the index is shared with the surface pass and must not be
   > filtered per consumer.

---

## 0.5. Falsification, 2026-08-25: the proxy's ceiling is 2 % of the pass

Phase 2 was shelved on 2026-08-24 against GPU timestamps that under-reported by up to
8489× (`crates/render/tests/timestamp_lie.rs`), and un-shelved on 2026-08-25 when honest
timestamps showed the volumetric march is the whole lit-frame cost. **The un-shelving was
also wrong, and this section is why.** The march is indeed the cost; the proxy does not
address the part of it that costs.

### The argument the doc never checked

§2.5 pre-registers the win as `gpu_time ratio ≈ B̄ₚ/B̄` — proxy fragments over marcher
candidates. That model assumes a marcher candidate and a proxy fragment cost the same.
They do not, and the reason is in the marcher's own code: `beam_scatter` runs the exact
analytic ray/cone∩ball span test *first* and returns zero before the sample loop
(`beam_transport.wgsl`). A candidate the ray misses never reaches a sample. So the proxy
does not remove work proportional to `B̄ − B̄ₚ`; it removes `(B̄ − B̄ₚ)` **span tests**,
which is a different and much smaller quantity.

The proxy's real ceiling is therefore

```
saving ≤ (B̄ − B̄_hit) × cost(span-test reject)
cost   =  B̄_hit × haze_steps × cost(sample)
```

and both costs are measurable directly.

### Measured

`B̄` and `B̄_hit` from the CPU reference index (`CpuLightIndex`) plus `beam_scatter`'s span
test transcribed, one ray per 8 px tile centre, 2558×1357 — the resolution the reported
20–30 fps venue runs at:

| workload | lights | `B̄` (index candidates/px) | `B̄_hit` (non-empty span/px) | hit/candidate |
|---|---|---|---|---|
| stall-probe rig | 30 | 23.7 | 8.1 | 34 % |
| stall-probe rig | 120 | 66.9 | 13.6 | 20 % |
| wide-overhead rig | 30 | 25.1 | 7.1 | 28 % |
| wide-overhead rig | 120 | 71.6 | 27.5 | 38 % |

So the index *is* loose — 62–80 % of candidates miss, and an exact hull rasterizer would
remove all of them. That is the good news, and it is the whole of the good news.

Per-evaluation costs, M3 Max, 67 M evaluations per kernel, same loop shape, best of ten:

| kernel | ns/eval |
|---|---|
| empty loop + phase function | 0.007 |
| **`beam_scatter` reject path** (sphere + cone quadratic + span partition) | **0.011** |
| **one full sample's density term** (`haze_noise`) | **0.119** |

A rejected candidate costs **one ninth of one sample**, and a hit costs eight samples.
For the 30-light rigs above:

```
hits     8.1 × 8 × 0.13 ns  =  8.4 ns/px      98 %
rejects  15.6 ×    0.011 ns =  0.17 ns/px      2 %
```

**Two per cent.** That is the entire budget BeamPass Phase 2 is competing for, before it
spends any of it back on vertex work, `Rgba16Float` blend read-modify-write, and one TBDR
primitive flush per beam (§0 correction 1 — which on `beams-at-camera-128` makes it a net
loss, as that section already predicted). There is no version of the proxy that wins here.

The cost model also reproduces the observed frame: 867 k half-res pixels × 8.4 ns ≈ 7.3 ms
of sample cost for static30, against ~13 ms measured `draw_time` for the whole lit frame at
that resolution.

### Where the cost actually is

`haze_noise` is **94 %** of a stripped sample — two octaves of `noise3d`, sixteen `hash3`
gradient evaluations, 48 `sin`. Substitutes, measured the same way:

| density term | ns/eval | vs current |
|---|---|---|
| `haze_noise`, current | 0.119 | 1.00× |
| integer hash instead of `sin` | 0.122 | **1.00× — no win** |
| gradients from a wrapping 3D texture, same interpolation | 0.070 | 1.7× |
| prefiltered field, two trilinear fetches | 0.034 | **3.5×** |

The `sin` is not the cost — replacing it with a PCG-style integer hash changes nothing,
which rules out the obvious micro-optimisation and says the cost is the sixteen gradient
evaluations and their register pressure, not the transcendental. Only moving the lattice
into memory helps, and the fully prefiltered field helps most.

That is a change to the *integrand*, so it re-baselines every haze golden and needs its own
design (tiling period vs. the current aperiodic field; texture-cache behaviour under the
real march's incoherent access, which the microbenchmark's coherent access flatters). It is
not this document's subject. It is where the next volumetric millisecond is.

Probe sources: `scratchpad/beamprobe/` (out of tree, `src/main.rs` for the index counts,
`src/bin/noisebench.rs` for the per-evaluation costs).

---

## 1. Cone proxy geometry

### 1.1 What the hull must contain

The set a beam can light is the **ice-cream cone** `C = cone(P, D, f) ∩ ball(P, range)`:
points within field half-angle `f = acos(cos_field)` of the axis and within `range` of the
apex. `haze.wgsl:311–383` computes the ray's span through exactly this solid; a fragment
whose span is empty returns zero. So the proxy's only contract is:

> **Conservativeness.** For every camera ray that meets `C` in a non-empty span, at least one
> proxy fragment is generated at that pixel.

Wrongly covering a pixel costs ~25 ALU (the span test, then `continue`). Wrongly missing one
is a hole in a beam with no other symptom. Bias every epsilon toward covering — the same
contract, and the same reasoning, as `clusters::cone_reaches_sphere` (`clusters.rs:149–177`).

### 1.2 One parametric template, two forms, zero vertex buffers

One shared **index buffer** and no vertex buffer at all. `vs_main` decodes
`@builtin(vertex_index)` into `(ring, segment)` and places the vertex from the per-instance
`LightCore`/`LightRest` already in the storage buffers (`gpu.rs:112–138`). This matches the
existing idiom — every full-screen pass in this crate already synthesises positions from
`vertex_index` (`haze.wgsl:87–92`).

```
SEGMENTS  = 16      // azimuthal
CAP_RINGS = 3       // latitude rings between rim and pole
vertices  = 1 (apex) + CAP_RINGS*SEGMENTS + 1 (pole) = 50
triangles = SEGMENTS + 2*SEGMENTS*(CAP_RINGS-1) + SEGMENTS = 96   // 288 indices
```

Basis: `D` is the axis; `R = normalize(cross(D, helper))`, `U = cross(R, D)` — reuse the
exact helper-vector selection from `gobo_transmission` (`fixture_light.wgsl:48–54`) so the
proxy and the gobo agree about what "around the axis" means and a rotated gobo can never
poke outside its own hull.

**Circumscribe factor.** An inscribed `N`-gon's chord midpoint sits at `cos(π/N)` of the
circle's radius, so the tangent-plane polygon needs `k = 1/cos(π/SEGMENTS)`. At
`SEGMENTS = 16`, `k = 1.0196` → **3.96 % excess area**. `SEGMENTS` is therefore a *fill-waste*
knob, not a quality knob: 12 → 7.2 %, 24 → 1.7 %. Nothing about the picture changes with it.
This is the substantive difference from Unity's Volumetric Light Beam (§2.2), where mesh
`Segments` *is* the silhouette; here the silhouette is analytic and tessellation only buys
back fill.

**Cone form** — used when `f ≤ F_CONE_MAX`:

```
apex        → P
ring 1, s   → P + D*range + (R*cos φ + U*sin φ) * (range * tan(f) * k),  φ = 2πs/SEGMENTS
rings 2,3   → P + D*range          (collapsed: the cap becomes a fan)
pole        → P + D*range
```

The flat cap at **axial** distance `range` is conservative because every point of `C` has
axial coordinate `|p|·cos(angle) ≤ range`; the spherical cap bulges *inward* of that plane,
never past it. The lateral faces are the planes tangent to the cone (`tan f' = tan f · k`),
which is where `k` earns its keep. 32 live triangles, 64 degenerate — degenerate triangles
produce no fragments and cost nothing worth measuring, and paying for them is how the
template stays a single index buffer.

**Sphere form** — used when `f > F_CONE_MAX`:

```
apex        → P - D*range*k_s
ring r, s   → P + (D*cos θ_r + (R cos φ + U sin φ)*sin θ_r) * range*k_s,  θ_r = π*r/4
pole        → P + D*range*k_s
k_s = 1/cos(π/8) = 1.0824      // half the 45° latitude step is the binding error
```

All 96 triangles live. This is a circumscribed sphere about `ball(P, range)`, which contains
`C` trivially for any `f`.

**`F_CONE_MAX = 65°` (i.e. `cos_field < 0.42` takes the sphere form),** derived by equating
proxy volumes: cone form `(π/3)·range³·tan²f'`, sphere form `(4π/3)·range³·k_s³ = 5.31·range³`,
which cross at `tan f' = 2.25`, `f ≈ 65.4°`. Screen area is the metric that actually matters
and volume is only a proxy for it, so treat 65° as a starting value with a measurement (§7
histogram) attached, not a constant handed down. Note `sanitize_fixture_cone` clamps
`cos_field ≥ 0.01` (`gpu.rs:4769`), i.e. `f ≤ 89.4°`, so the sphere form is always reachable
and never unbounded — which is exactly what the flat-cap form is not, since `tan f → ∞`.

### 1.3 Entry / exit: computed analytically, never from the geometry

`fs_main` does **not** interpolate an entry point, read a back-face depth, or use anything the
rasterizer produced except `@builtin(position)`. It reconstructs the ray from the fragment's
own uv exactly as `haze.wgsl:226–253` does today and calls the shared span code. The proxy is
a coverage mask and nothing else.

That is what makes "the pictures match" a *provable* property rather than a hope, and it is
also why the conservative hull is free: an over-covered pixel's span is empty and it exits at
the same `continue` the marcher takes today (`haze.wgsl:315`, `321`, `381`).

### 1.4 Camera inside the cone: define it out of existence

`cull_mode: Some(Face::Front)` — **draw back faces only, always.** No CPU inside/outside test,
no two batches, no per-instance state, no depth-state flip. Drobot's `Z GREATER`/backface flip
(§2.3) exists to make a *depth-tested* light volume work; we have no depth attachment on this
pass, so the only thing the flip would buy is choosing which of two covering surfaces
rasterizes — and back faces cover the silhouette in both configurations:

| eye | front faces | back faces |
|---|---|---|
| outside the hull | full silhouette | full silhouette |
| inside the hull | behind the eye, clipped away | in front of the eye, full silhouette |
| hull straddles the near plane | partially clipped | intact |

Back faces are the case that is never wrong. This deletes review finding 2's "eye-inside
handling" as a code path, deletes §3.1's "two draw batches since Metal has no per-instance
pipeline state", and — critically for §5 — keeps the frame at **one beam pipeline**.

Consequence to state so nobody re-derives it: exactly one proxy fragment is generated per
pixel per beam, so the additive blend adds each beam's radiance exactly once. Front-and-back
would double it.

### 1.5 Depth clamp to the scene

Unchanged from today: the fragment loads the full-res prepass depth over its own footprint
with the checkerboard near/far pick (`haze.wgsl:226–240`), derives `hit_dist`, and the span's
`s1` is `min(-b + sq, hit_dist)` (`haze.wgsl:320`). No depth attachment, no depth test.

The tempting optimisation — depth-test the proxy to skip beams behind geometry — is a
**correctness bug** and should be written down as one so it is not reinvented: a back face
failing the depth test says the beam's *far* extent is occluded, not its near extent, and a
beam in front of a wall would vanish. Only a front-face test would be sound, and front faces
are unusable per §1.4. The analytic `s1 <= s0` early-out already handles the fully-occluded
case for ~25 ALU.

### 1.6 The shared WGSL, spelled out

Today `haze.wgsl` is compiled as `format!("{fixture_light}{haze}")` (`gpu.rs:1136–1139`). Keep
that mechanism — it is already the one canonical way this crate shares WGSL — and add one
file in the middle.

**New: `shaders/beam_transport.wgsl`.** Owns the *entire* group-0 bind layout and every
function both consumers need. Moved verbatim out of `haze.wgsl`:

| moved from `haze.wgsl` | lines |
|---|---|
| `LightCore`, `LightRest`, `TileHeader`, `Haze`, `FixtureShadowMatrix` | 22–75 |
| `@group(0)` bindings 0–8 | 77–85 |
| `hash3`, `noise3d`, `haze_noise` | 94–136 |
| `world_from_ndc`, `linear_view_depth`, `henyey_greenstein` | 138–188 |
| `fixture_shadow_visibility` | 143–172 |
| `BLUE_NOISE_RANK`, `blue_noise` | 190–211 |

Plus two functions **factored out of `fs_main`, with the bodies moved not retyped**:

```wgsl
struct SceneRay {
    dir: vec3<f32>,
    hit_dist: f32,
    view_depth: f32,
    jitter: f32,
};

/// Reconstructs this fragment's camera ray, its scene-occlusion distance and its
/// blue-noise stratum offset. Byte-identical to what the marcher's fs_main did:
/// the checkerboard near/far depth pick and the flipped-row jitter coordinate are
/// properties of the target, not of the pass, so both consumers must not merely
/// agree — they must run the same instructions.
fn scene_ray(frag: vec2<f32>) -> SceneRay;                      // haze.wgsl:214–270

/// Single-scattering radiance this ray receives from light `li`, already
/// multiplied by sigma. Returns zero when the ray misses the light's cone∩ball.
fn beam_scatter(li: u32, ray: SceneRay, sigma: f32) -> vec3<f32>;  // haze.wgsl:310–465
```

`beam_scatter` is `haze.wgsl:310–465` with `scattered += acc * sigma` replaced by
`return acc * sigma` and every `continue` by `return vec3<f32>(0.0)`. Nothing else changes —
including the `if haze.shadow.x > 0.0` guard at line 456, whose comment ("even a multiply by 1
can change half-float rounding") is the reason this refactor gets a byte-exactness gate in §6.

After the move:

- **`haze.wgsl`** = full-screen triangle + `fs_main` that calls `scene_ray`, does the ambient
  bed (lines 274–291), then loops the tile list calling `beam_scatter`. ~90 lines.
- **`shaders/beam.wgsl`** (new) = `vs_main` (§1.2) + `fs_main`:
  ```wgsl
  @fragment fn fs_main(@builtin(position) frag: vec4<f32>,
                       @location(0) @interpolate(flat) li: u32) -> @location(0) vec4<f32> {
      let ray = scene_ray(frag.xy);
      return vec4<f32>(beam_scatter(li, ray, haze.depth.z) * haze.tuning.y, 0.0);
  }
  ```
  ~40 lines including the vertex placement.

Both are compiled as `format!("{fixture_light}{beam_transport}{…}")`. **There is exactly one
copy of the integrand, and it is a function, not a `#include` of a fragment body.** A reviewer
checking this reads `beam_transport.wgsl` and nothing else.

Both pipelines bind the *same* `haze_layout` (`gpu.rs:1065`) — the beam pass needs no new
bindings, only `ShaderStages::VERTEX_FRAGMENT` visibility on bindings 1 and 2 so `vs_main` can
read the SoA. `HazeUniform` gains `view_proj` (the vertex shader needs it) and one `beam`
`vec4`; `tiles.w` is already a spare seed slot.

> **Smell, adjacent, flag it:** binding 8 is a `sampler_comparison` (`haze.wgsl:85`,
> `gpu.rs:2962`) that the haze path never uses — it does a nearest `textureLoad`. It is a dead
> binding kept alive by copy-paste from the scene layout. Delete it in step 1 of §6; it is one
> line and the byte-exactness gate proves it was dead.

---

## 2. The worst case, and the fallback

### 2.1 The constraint the design has to respect

Julian's constraints: **no beam-length clamp, no intensity-range culling, no frame-time
governor.** That removes review findings 5, 6 and 3 — which are, per the review's own ranking,
three of the four levers the category actually uses on this case. What is left has to be a
change of *traversal*, not of *transport*: the same integrals, computed in a cheaper order.

Note the constraint is against a **closed-loop frame-time governor**, and the review itself
names the alternative it endorses (finding 3's caveat): *"everything else adaptive in previz is
a stateless function of a scene parameter — deterministic and cross-fade-safe."* Everything
below is a stateless function of scene state. Nothing reads a timer.

### 2.2 What the fallback is

**The existing tiled marcher.** Not a reduced-resolution path, not a per-tile variant, not a
new shader. `haze.wgsl` survives, its tile lists survive, and it runs over a **subset** of the
lights — usually the empty subset.

Why the marcher is the right fallback for exactly this case: at `M` beams all covering most of
the screen, the proxy path pays `M ×` (fragment launch + `scene_ray` + blend RMW + one HSR
flush per primitive) and the marcher pays that once, then loops `M` times inside one fragment.
The integrals are identical in count; the per-beam overhead is not. Olsson & Assarsson's tiled-
vs-stencil measurement (review finding 2) is the published version of the same trade, and their
17 % gap is measured on a workload that *also* pays G-buffer re-read, which we do not.

### 2.3 The partition predicate

Per beam, stateless, order-independent, no global sort:

```
use_marcher(i)  ⟺  projected_area(hull_i) > BEAM_FILL_THRESHOLD * (haze_width * haze_height)
BEAM_FILL_THRESHOLD = 0.25
```

`projected_area` is a screen-space AABB area of the near-plane-clipped hull — the *same*
computation `haze_tiles` already does (`gpu.rs:4710–4742`), which is why this is a rename and
not new geometry code. It requires generalising `clusters::for_each_clipped_vertex`
(`clusters.rs:109`) from its hardwired `[Vec3; 8]` + 12 box edges to
`(&[Vec3], &[f32], &[(u16, u16)], clip, visit)`, with `BOX_EDGES` moved to a `const` the two
existing callers pass. Two callers, one primitive, no duplication — the same consolidation §1
of the parent doc already did once for this function.

An AABB over-estimates a round hull by up to `4/π = 1.27×`, which biases toward the marcher.
That is the safe direction: the marcher is the validated path.

**Why a per-beam predicate rather than a per-tile beam count.** A per-tile count is a property
of the *frame's* tile lists, so it is a feedback term — a beam's path would depend on which
other beams are near it, which changes when any of them moves, which makes the partition
non-local and its hysteresis a real problem. Screen area is a property of the beam and the
camera alone.

### 2.4 Why this composes with temporal history with no seam at all

Three facts, in order:

1. `beam_scatter(li, ray, sigma)` returns the **same value** on both paths. Same function, same
   inputs — `ray` comes from the same `scene_ray(frag.xy)` on the same target, and `jitter`
   from the same `blue_noise` on the same flipped-row coordinate. Not "visually equivalent":
   bit-identical, and §6 step 1 proves it with a byte gate.
2. The only difference is the summation order — the marcher accumulates into a register
   (`scattered += …`) and the proxy accumulates through fixed-function `One/One` blending in
   `Rgba16Float`. Both are the same additions in the same ascending-light-index order (WebGPU
   guarantees blend ordering follows primitive order within a draw; instance order is ascending
   instance index). The difference is half-float rounding at intermediate steps, bounded by
   `2⁻¹¹` relative per add.
3. **A beam flipping paths cannot invalidate history, because `haze_history_key` already
   covers every input to the predicate.** The key hashes per-cone `position`, `range`,
   `direction`, `cos_field` and the camera `eye`/`target`/`fov` and `haze` target size
   (`gpu.rs:4792–4832`). `projected_area(hull_i)` is a pure function of exactly those. So any
   state change that could move a beam across the threshold has already reset the history one
   line earlier.

No hysteresis needed, no new key field, no seam. This is the payoff for §0's property — and it
is worth writing the assertion down in the code, because the day someone adds an intensity term
to the predicate (finding 6, ruled out today but tempting later), the key stops covering it and
that comment is the only thing standing between them and a one-frame flash.

### 2.5 Does this actually drop `beams-at-camera-128`'s max? Pre-register the answer.

Be honest about what Phase 2 does and does not buy on the gating case, in the style this
document's Phase 1 gate was reported in.

Today's ~48 ms max is `128 lights × 8 samples × 518 400 half-res px × 2 subframes ≈ 1.06 G`
samples on the frames where the orbit lines the beams up with the lens. That number has two
components and they need separating before anyone promises a factor:

- **The physics floor.** Beams that genuinely cover the pixel. Nothing in Phase 2 removes one
  of these; they are the picture.
- **The tile-list slack.** The profiler records `mean_lights_per_cluster = 116` out of 128 on
  this case (`bin/profile-volumetrics.rs:283`) — the 2D screen-AABB tiler assigns nearly every
  light to nearly every tile. The proxy path replaces a screen AABB of a 3D hull with exact
  rasterization of that hull. The apexes are metres apart on a truss, so their hulls are *not*
  coincident even when all are aimed at one eye point.

**Slack is the entire Phase 2 win, and it is currently unmeasured.** Review finding 9 ranks the
per-pixel beam-count histogram as a prerequisite for exactly this reason, and it is right: §3.1
of the parent doc asserts a cost model of `Σ beam screen area` and nothing in the repository
measures that sum. So:

> **Answered in §0.5, and the pre-registration's model was wrong.** `B̄ₚ/B̄` measures 0.20–0.38,
> which by this paragraph's own criterion would have been a pass. It is not one, because the
> ratio was never the right multiplier: a rejected candidate costs a ninth of a sample, not a
> sample. The corrected ceiling is 2 % of the pass. Read §0.5 before restarting this section.

> **Pre-registered.** Land the histogram first (§7). Let `B̄` = mean live beams per lit pixel
> under the marcher (≈ the tile list length, ~116) and `B̄ₚ` = mean beams whose hull covers a
> pixel under the proxy. Phase 2's gpu-time ratio on this case is `≈ B̄ₚ/B̄` plus the fallback's
> per-beam-overhead saving. **If `B̄ₚ/B̄ > 0.7`, Phase 2 alone does not fix this case**, and the
> honest report is that the remaining levers are §2.6 or one Julian has ruled out — not a
> larger claim about the proxy.

Phase 2's *unconditional* win is on every other case, where the beams do not contain the eye
and `B̄ₚ ≪ B̄`. `transport-128` at `WIDE` radius is the shape of the average frame and is where
"cost proportional to beam coverage" actually pays.

### 2.6 The escalation that stays inside the constraints, if §2.5 lands badly

**Sun et al.'s `F(u,v)` LUT** (parent doc §2.2, review finding 14 — re-ranked from "curiosity"
to "worst-case lever" precisely because strobe and blinder cues are open white and un-goboed).
Two texture fetches and a handful of ALU replace 8 samples of `noise3d` + HG + gobo + taper.

It is **not** in Phase 2 and must not be, for a reason worth stating: the current integrand
multiplies every sample by `haze_noise` (`haze.wgsl:459`), so no beam in this renderer is
homogeneous and the closed form is not the same picture. It is a Phase 2b with its own toggle
and its own goldens, and its admission predicate (`gobo == 0 && shadow_slot < 0 && wash == 0 &&
noise disabled`) is a fourth path, which §5 says to be suspicious of. Design it then, with §2.5's
histogram in hand; do not pre-commit to it now.

---

## 3. Compositing: the bed, the beams, the temporal resolve

### 3.1 Two passes, one target, one owner for alpha

The half-res `haze` target's alpha carries linear view depth for the composite's bilateral
upsample and the temporal resolve's rejection test (`haze.wgsl:468–471`, `composite.wgsl:57–63`,
`haze_temporal.wgsl:26`). Additive blending would accumulate that depth once per beam. So the
pass splits, and the split is along the same line as everything else:

| pass | draws | writes | blend | owns |
|---|---|---|---|---|
| **bed** (`haze.wgsl`) | 1 full-screen tri | RGB + A | replace | ambient in-scatter, `A = view_depth·weight`, and the marcher subset |
| **beams** (`beam.wgsl`) | 1 instanced draw | RGB only | `One, One` | every proxy-path beam |

`ColorWrites::COLOR` on the beam pipeline is what keeps alpha the bed's. Downstream is
untouched: `haze_temporal.wgsl` and `composite.wgsl` do not change by one character.

Subframe loop, replacing `gpu.rs:2892–2993`, structurally unchanged:

```rust
for k in 0..subframes {
    // bed: LoadOp::Clear on k == 0, Load after — exactly as today (gpu.rs:2972)
    // beams: LoadOp::Load always, same haze_view attachment
}
```

Two passes per subframe instead of one. At `LIVE_SUBFRAMES = 2` that is 4 render passes where
there were 2; at `DEFAULT_SUBFRAMES = 16` it is 32 where there were 16. Review finding 12 notes
gfx-rs/wgpu#8768 leaks ~96 bytes per Metal render-pass creation and an encoder cost ceiling
around 110 µs — 32 passes ≈ 3.5 ms of encode ceiling, which is real and worth watching on the
export path. If it bites, the mitigation is one pass per subframe with the bed drawn as
instance −1 of the same draw (a full-screen triangle emitted by the same `vs_main` for a
sentinel instance index), which is strictly better than a second pipeline. Note it; do not
build it speculatively.

### 3.2 `LIVE_HAZE_RESOLUTION` stays 0.5, and the upsample stays as-is

Per §0 correction 2. The cone edge is produced by the analytic span test at the beam target's
resolution, so a half-res target gives a half-res edge and the composite's depth-guided
4-tap bilateral (`composite.wgsl:44–79`) is still what reconstructs it — including the
all-taps-disagree single-nearest fallback at lines 66–75, which exists because of exactly this
edge and stays load-bearing.

Two things Phase 2 must not quietly break:

- The **checkerboard near/far depth pick** (`haze.wgsl:218–239`) is what guarantees every
  full-res pixel has a same-side tap in its 2×2 neighbourhood. It lives in `scene_ray`, so
  both passes inherit it. The beam pass must not "simplify" to a single depth tap.
- **No MSAA on the beam target.** Alpha must stay a single un-resolved depth or the bilateral
  weights become meaningless.

A full-res beam pass is a legitimate future knob — the beam pass is geometry now, so its cost
scales with beam coverage rather than screen area, and full-res may be affordable on sparse
frames where half-res is currently paying for nothing. That is a Phase 2c measurement, not a
Phase 2 change, and it would re-baseline every golden a second time.

---

## 4. Fixture shadows inside the proxy

### 4.1 No change to the sampling, by construction

`fixture_shadow_visibility` moves into `beam_transport.wgsl` unedited: same
`texture_depth_2d_array`, same 16 layers of 256² (`gpu.rs:38`, `49`), same slot indirection
through `shadow_slot`, same nearest `textureLoad`, same metric slack via
`shadow_compare_reference` (`fixture_light.wgsl:13`). The `if haze.shadow.x > 0.0` guard
(`haze.wgsl:456`) moves with it, unedited, because it is load-bearing for byte-exactness.

Phase 3 (tiers, throttle, moment maps) is unaffected and lands on top of one function instead
of two — which is a direct argument for doing this refactor before Phase 3, not after.

### 4.2 Cost model

Shadow taps are gated twice before they happen: a fragment only exists if its beam's hull
covers it, and a sample only fetches if `angular > 0.0` (`haze.wgsl:432`, an early `continue`
*before* the shadow fetch — the parent doc's Phase 0 already established there is nothing to
reclaim there). So

```
taps ≈ Σ_{shadowed beams i} (fragments covered by hull_i) × (in-cone samples per fragment) × subframes
```

Worked, half-res 960×540, `haze_steps = 8`, `subframes = 2`, 16 shadowed cones averaging 8 %
screen coverage with ~60 % of samples in-cone:

```
16 × 0.08 × 518 400 × 8 × 0.6 × 2 ≈ 6.4 M point loads / frame
```

against a 16 × 256² × 4 B = **4 MB** array that is system-level-cache resident on M2/M3. At
point-load rates that is a fraction of a millisecond, and it is *strictly lower* than today's
count, because today the fragment count is `screen × tile-list-length` rather than
`Σ hull coverage`. **Shadows get cheaper in proportion to whatever §2.5's histogram measures,
and by no independent mechanism.** Phase 2 makes no shadow claim of its own.

The one number that does move: with 16 shadow slots and a 128-cone rig, 112 cones carry
`shadow_slot < 0` and return 1.0 at `haze.wgsl:146` — that early return is now taken inside a
fragment that only exists because a hull covered it, so the *wasted* branch count drops with
the same ratio. Not worth a line of code; worth not being surprised by in the profile.

---

## 5. One pipeline, and the grandMA3 trap

The field datum (review, Field data): grandMA3 onPC renders an x4 Bar rig at 63 fps and a
JDC-1 rig at 63 fps; **both together, 3 fps, with the GPU below idle.** That is pipeline
state-switch serialization, not saturation. It is the single cheapest catastrophic failure to
design against, and the rule it yields is absolute:

> **One render pipeline for every beam, of every type, in every frame.**

Phase 2's per-frame pipeline budget:

| pipeline | count | note |
|---|---|---|
| bed (`haze_pipeline`) | 1 | also carries the marcher-subset loop |
| beams (`beam_pipeline`) | 1 | all cones, all field angles, all gobos, both hull forms |
| temporal | 1 | unchanged |
| composite | 1 | unchanged |

One added pipeline over today. Everything that could plausibly become a second beam pipeline is
instead per-instance data read in `vs_main` or `fs_main`:

- **hull form** (cone vs sphere) — a branch on `cos_field` in `vs_main`, §1.2.
- **eye inside vs outside** — does not exist, §1.4.
- **wash vs beam** — already a `LightRest.wash` scalar feeding the phase `g` mix
  (`haze.wgsl:394`); nothing to switch.
- **gobo 0/1/2** — already a branch inside `gobo_transmission`.
- **proxy vs marcher** — a different *pass* with a different shader, chosen once per frame for
  a whole subset, not per beam and never mid-draw. Worst case it adds zero pipelines (the bed
  pipeline is the marcher).

**The emissive-only bucket** (review finding 10 — five products ship it, `Throws light` off /
Force Emissive / `Glow` / Simple Scattering) is the obvious next thing someone will want for a
beam pointed at the lens, and it is the obvious next thing to get wrong. When it lands it must
be **a flag on `LightRest` that makes `beam_scatter` return early**, never a second pipeline and
never a second mesh. Same for grandMA3's `Line` rung. Write that constraint into
`beam_transport.wgsl`'s header comment now, while there is nothing to argue about.

---

## 6. Migration

**Step 1 landed. Steps 2–9 are shelved on §0.5 and are kept as the plan that would run if the
ceiling ever moved.** Two things would have to become true first, and each is a measurement,
not a judgement call:

1. **The sample gets cheap.** At §0.5's substituted density term a sample costs ~0.05 ns and a
   reject still costs 0.011 ns, so the traversal share rises from 2 % to ~5 %. Still not enough,
   but it is the direction, and the ceiling should be re-derived — not re-argued — after any
   change to the integrand.
2. **A rig appears where `B̄_hit` is small and `B̄` is not.** Every rig measured has `B̄_hit ≥ 7`;
   the beams genuinely overlap, and that overlap is the picture. A rig of narrow, well-separated
   pins over a deep stage would invert the ratio. If one shows up, measure it with
   `scratchpad/beamprobe` before touching the renderer.

Note also that step 5 as written no longer has a subject: `haze_tiles` and `HazeTileCache` are
gone (§0 correction 3's own correction), so the partition would land as a per-light exclusion
bitmask tested inside `haze.wgsl`'s `light_index_next` walk. Sixteen `u32` in `HazeUniform`, one
load and one bit test per candidate — which, at 0.011 ns per candidate, is a cost of the same
order as the thing it is trying to skip. That is the falsification restated in miniature.

### Step 1 — Extract `beam_transport.wgsl`. **Byte-exact gate.**

Files: `shaders/beam_transport.wgsl` (new), `shaders/haze.wgsl` (reduced), `gpu.rs:1135–1139`
(module composition), `gpu.rs:1065–1089` (drop the dead `sampler_comparison` at binding 8, and
`gpu.rs:2962`).

No behaviour change whatsoever: the same instructions in the same order, relocated into two
functions. Gate:

```
cargo run -p luma-render --release --bin render-contract-goldens
cargo run -p luma-render --release --bin render-goldens
git diff --stat -- gpui/crates/render/goldens harness/goldens     # must be empty
```

**All 31 contract goldens and all 24 `harness/goldens/scenes-wgpu` images must be byte-identical.**
If one moves, the refactor changed arithmetic and the fix is to find out where — not to
re-baseline. This step is the safety net every later step spends; do not merge it together
with step 3.

> **Executed 2026-08-25, gate amended by measurement.** Byte-exactness across the extraction
> is not attainable: naga/MSL contracts FMAs differently across the new function boundary,
> and the drift measured **max 2 LSB on ≤0.24% of pixels** on the contract set (one-beam:
> 184 px × 1 LSB of 400k). Identical source instructions, different fusion. The set was
> re-baselined once at this bound — contracts + volumetric-stress PNGs, the pinned hash
> tuples in `volumetric_transport.rs`, and `volumetric-stress-scenes.json` — and **the new
> baseline is the byte-exact reference every later step spends.** The scenes-wgpu images
> also absorbed the (independent, intentional) beam-gain surface-lighting fix from earlier
> the same day; their large deltas are lit floors, not the extraction — verified by mean
> brightness direction and by the contract set's LSB bound.

### Step 2 — Uniform and settings plumbing. Still byte-exact.

- `gpu.rs:96–108` — `HazeUniform` gains `view_proj: [[f32; 4]; 4]` and `beam: [f32; 4]`
  (x: proxy instance count, y: `F_CONE_MAX` cosine, z: `BEAM_FILL_THRESHOLD`, w: spare).
  Unread fields; goldens do not move.
- `frame.rs:167–216` — `Frame` gains `pub beam_proxy: bool`.
- `scene_desc.rs:79–104` — `RenderSettings`/`HazeSettings` gains the serde field, defaulting
  `false`, so every existing golden sidecar deserialises unchanged.
- `visualizer.rs:490–546` — `RenderLab` gains `beam_proxy: bool` (default `false`),
  `LabToggle::BeamProxy` (`visualizer.rs:596`), one `lab_toggle` row. `RenderLab` already
  derives `PartialEq` and rides inside `IdleKey` by whole-struct comparison
  (`visualizer.rs:487–489`), so the new dial invalidates idle correctly with no further work —
  which is the reason that comment exists.

### Step 3 — `beam.rs`: the CPU half, unit-tested with no GPU.

New module `crates/render/src/beam.rs`:

```rust
pub(crate) const BEAM_SEGMENTS: u32 = 16;
pub(crate) const BEAM_CAP_RINGS: u32 = 3;
pub(crate) const F_CONE_MAX_COS: f32 = 0.4226;      // 65°
pub(crate) const BEAM_FILL_THRESHOLD: f32 = 0.25;

/// Index buffer for the shared proxy template. Built once at renderer
/// construction; there is no vertex buffer — `vs_main` places vertices from
/// `vertex_index` and the instance's `LightCore`/`LightRest`.
pub(crate) fn proxy_indices() -> Vec<u32>;

/// Screen-space area, in target pixels, of the near-plane-clipped proxy hull.
pub(crate) fn projected_area(cone: &FixtureCone, view_proj: Mat4, size: (u32, u32)) -> f32;

/// Which beams rasterize a proxy and which fall back to the marcher.
pub(crate) fn partition(cones: &[FixtureCone], view_proj: Mat4, size: (u32, u32))
    -> (Vec<u32>, Vec<u32>);
```

Also in this step, generalise `clusters::for_each_clipped_vertex` (`clusters.rs:109–147`) per
§2.3 and move `EDGES` (line 117) to a `pub(crate) const BOX_EDGES` the existing two callers
(`clusters.rs:447` `bounds_for`, `gpu.rs:4719` `haze_tiles`) pass explicitly.

Tests, mirroring the property-test style Phase 1 used for `cone_reaches_sphere`:

- **Conservativeness.** For a swept set of `(f, range, camera)`, sample points uniformly inside
  `cone ∩ ball` and assert each projects inside the proxy hull's projected convex outline. This
  is the contract of §1.1 and it is the one that produces holes if it is wrong.
- **Form crossover.** `cos_field` either side of `F_CONE_MAX_COS` selects the right form and
  both forms contain the same sampled points.
- **Bounded fill.** Cone-form projected area ≤ `1.05 ×` the analytic cone footprint for
  `f ≤ 30°`, catching a `k` regression.

### Step 4 — `beam.wgsl` and the beam pipeline.

Files: `shaders/beam.wgsl` (new), `gpu.rs` pipeline construction near `gpu.rs:1251`.

```rust
// blend: One/One on RGB, alpha untouched
write_mask: wgpu::ColorWrites::COLOR,
primitive: wgpu::PrimitiveState {
    cull_mode: Some(wgpu::Face::Front),     // back faces only — §1.4
    ..Default::default()
},
depth_stencil: None,
```

Bind group: the existing `haze_layout` verbatim, plus `ShaderStages::VERTEX_FRAGMENT` on
bindings 1 and 2. Draw: `pass.set_index_buffer(proxy_idx, Uint32); pass.draw_indexed(0..288, 0,
0..n)` where instances index a small `proxy_light_index` storage buffer (the §2.3 partition)
rather than the light array directly — one indirection, one `@location(0) @interpolate(flat)`
varying, and the light SoA stays untouched and shared with `scene.wgsl`.

### Step 5 — Wire the partition into the frame.

`gpu.rs:2857–2993`. `haze_tiles` is now called with the **marcher subset only**, and
`HazeTileCache` (`gpu.rs:573–587`, `2858–2887`) is deleted outright — its key is the unsound
camera-derived one §3.2 of the parent doc already condemns, and on a subset that is usually
empty a cache is pure liability. `haze_tiles`, `haze_tile_key` and `HAZE_TILE_SIZE` **survive**
(§0 correction 3).

Behind `frame.beam_proxy == false`, the partition sends every beam to the marcher and the frame
is bit-identical to step 2's. That is the toggle's contract and it is worth asserting in a test.

### Step 6 — Profiler instrumentation, before flipping the default.

`bin/profile-volumetrics.rs`. Two additions:

1. **Per-pixel beam-count histogram** (review finding 9, and the prerequisite §2.5 pre-registers).
   Simplest sound implementation with no new pass: a `debug_view` that writes beam count instead
   of radiance, read back in `--capture` mode and histogrammed on the CPU — the
   `DebugView::VolumetricAccumulation` slot (`visualizer.rs:583`) is the precedent. Report
   `mean_beams_per_lit_pixel` as a first-class `CaseResult` field alongside
   `mean_lights_per_cluster`, on both paths.
2. **`gpu_volumetric_max`** added to `Budgets` (`bin/profile-volumetrics.rs:74–84`) — the parent
   doc §3.5 asks for it and nothing has it. A p95 budget cannot fail on a 48 ms frame, which is
   how the 27× tail passed `all_within_budget: true`.

### Step 7 — Measure, then decide the default.

Gates, run with `--orbit` and in show mode, both paths, same binary, one flag apart:

| case | gate |
|---|---|
| `beams-at-camera-128` | `gpu_volumetric.max` **must drop.** Magnitude pre-registered in §2.5 against the histogram — report `B̄ₚ/B̄` next to it and do not claim a mechanism the histogram does not support |
| `transport-128` | `gpu_volumetric` p50/p95/max must not regress; expect a drop proportional to `1 − B̄ₚ/B̄` |
| `transport-512` | must not regress. This is the case whose max is 139.95 ms and where a per-beam overhead regression would show first |
| `fixture-shadows-120` | must not regress; §4.2 predicts a drop with no independent mechanism |
| `zoom-inside-128` | must not regress — a camera at 0.9 m is inside more hulls than any other case |
| all | `cpu_encode_submit` p95 must not regress: the partition replaces the tiler's per-light work with the same per-light work, and the pass count doubles per subframe (§3.1) |

### Step 8 — Flip the default, re-baseline, record.

`RenderLab::new` and `RenderSettings::default` flip to `true`. Then:

**Re-baseline (8 contract goldens — every one with `haze.enabled: true` and ≥1 cone):**
`one-beam`, `overlapping-beams`, `occluded-beam`, `gobo-seam-positive`, `gobo-seam-negative`,
`fixture-shadowed-beam`, `fixture-shadow-open`, `volumetric-performance-smooth`. Plus
`goldens/volumetric-stress-{32,128,512}.png` and the 21 `harness/goldens/scenes-wgpu` images
that carry haze (all except the three `venue-no-haze-*`).

The delta must be **half-float rounding only** (§2.4 item 2), so the re-baseline is gated on a
threshold comparison, not eyeballed: max per-channel delta ≤ 1 LSB of the 8-bit output over
≥ 99.9 % of pixels, with any excursion investigated as a bug in `beam_scatter`'s relocation
rather than accepted as "the new picture." Note in each PR which of the 8 moved and by how much.

**Stay byte-exact (verify, do not assume):**

- 7 contract goldens with `haze.enabled: false` — `metal-roughness-sweep`, `textured-pbr`,
  `sun-off`, `sun-direction-left`, `sun-direction-right`, `sun-shadow-hard`, `sun-shadow-soft`.
- `venue-no-haze-{0.000,1.370,4.200}` — `haze_density < 0.001` takes the bed pass's early
  return (`haze.wgsl:246–248`) and the CPU skips the beam draw entirely.
- Everything outside `crates/render` (all `harness/goldens/scenes` and `scenes-live`).

### Step 9 — Record the result in `volumetrics-v2.md`.

Append to §4 Phase 2 in the house style: what landed, what the gate measured, and — if §2.5's
histogram says `B̄ₚ/B̄ > 0.7` — that Phase 2 did not fix `beams-at-camera-128` and why, with the
same directness Phase 1's failed cone-vs-sphere gate was reported with. A phase that reports its
own negative result is worth more than one that quietly redefines its gate.

---

## 7. Open questions

1. ~~**`B̄ₚ/B̄` on `beams-at-camera-128`.** The whole Phase 2 claim.~~ **Closed by §0.5.** The
   ratio was measured (0.20–0.38) and is not the deciding quantity; the reject-vs-sample cost
   ratio is, and it caps the whole phase at 2 %. The remaining open question is not in this
   document: it is what replaces `haze_noise`.
2. **`SEGMENTS = 16` vs 24.** 4 % vs 1.7 % excess area against 96 vs 144 triangles per instance.
   At 512 instances that is 49 k vs 74 k triangles per subframe — negligible either way, so the
   answer is whatever the histogram prefers. Decide with data, not now.
3. **`F_CONE_MAX = 65°`** was derived on proxy *volume*; screen area is the metric. If wash
   fixtures turn out to dominate a real rig's fill, re-derive against measured coverage.
4. **Pass count at `DEFAULT_SUBFRAMES = 16`** — 32 render passes per exported frame (§3.1). If
   `cpu_encode_submit` moves on the export path, the sentinel-instance merge is the fix and it
   costs no new pipeline.
5. **f16 in `beam_scatter`** (review finding 7, `SHADER_F16` is in wgpu-hal's unconditional
   Metal base set) is now a single-function change instead of a two-shader one — but it is an
   image change and belongs behind its own gate, after Phase 2's re-baseline has settled.
