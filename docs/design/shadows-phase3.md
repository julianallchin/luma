# Shadows Phase 3 — tiers, refresh budget, moment maps, caster submission

Companion to `volumetrics-v2.md` §2.4, §3.3, "Phase 3", §8 and §9. Scope: the
per-fixture shadow path only (`gpu.rs`'s `fixture_shadow_*`, `scene.wgsl`'s
`fixture_shadow_visibility`, `beam_transport.wgsl`'s `fixture_shadow_visibility`).
The three sun cascades are out of scope and stay on depth + PCF — see §3.6 for
why that divergence is deliberate rather than debt.

No code changes here. Line references are against the tree as of
2026-08-25 (post-`LightIndex`, `clusters.rs` deleted).

---

## 0. Recommendation, in one paragraph

**Split residency from refresh.** Today one number, `MAX_FIXTURE_SHADOWS = 16`
(`gpu.rs:51`), is simultaneously the memory budget, the number of fixtures that
cast at all, and the number of maps rendered per frame. Phase 3 makes those
three separate numbers: 64 resident maps across three resolution tiers, ≤12
rendered per frame, and a hard rule that a map too stale to be honest is dropped
rather than shown. Then swap depth+PCF for 4-moment MSM so a lookup is one
bilinear tap instead of nine comparisons (surface) or one nearest `textureLoad`
(beam), and collapse the shadow pass's per-draw submission from one draw per
*caster* to one draw per *distinct mesh* per map. The measured target is
`dense-geometry-120` / `zoom-dense-geometry-120`, which fail `cpu_encode_p95`
at 4.6–4.9 ms against a 4.0 ms budget; the submission change is the only item
in this phase that attacks that number, and §5 shows it is worth ~1.5 ms.

Ordering matters: the submission change lands **before** MSM, because it is
gated bit-identical and MSM is the step that forces a golden re-baseline.

---

## 1. What exists today, precisely

Worth stating because two items in the doc's Phase 3 list are already landed in
a different form than planned, and one constant does three jobs.

| thing | where | value |
|---|---|---|
| map array | `gpu.rs:4748 fixture_shadow_texture_array` | one `texture_2d_array`, `Depth32Float`, 16 layers of 256² (`FIXTURE_SHADOW_SIZE`, `gpu.rs:40`), 4 MiB |
| residency | `gpu.rs:4448 assign_shadow_slots` | priority = `intensity · r² / d²`, `EVICTION_MARGIN = 1.25` hysteresis, slot identity = array position |
| slot ↔ cone | `Renderer::fixture_shadow_slots: [Option<usize>; 16]` (`gpu.rs:379`) | cone index per slot, carried across frames |
| validity | `ShadowCacheKey { matrix_bits, caster_hash }` (`gpu.rs:137`) | any pan/tilt changes `matrix_bits` ⇒ dirty |
| refresh | `gpu.rs:2379 fixture_shadow_dirty` | **every** dirty resident redraws; no per-frame cap |
| caster cull | `gpu.rs:2502 shadow_casters` | `light_index::cone_reaches_sphere` per (map, opaque draw), CPU |
| submission | `gpu.rs:2557 draw_range` | one `set_bind_group(1, materials[i])` **plus** one `draw_indexed` per caster |
| pass | `gpu.rs:2597` | one `begin_render_pass` per dirty slot, depth-only, `vs_depth` (`scene.wgsl:61`) |
| surface sampling | `scene.wgsl:119` | 3×3 `textureSampleCompareLevel`, 9 taps |
| beam sampling | `beam_transport.wgsl:131` | one `textureLoad`, nearest, hard edge softened by temporal accumulation |
| shared slack | `fixture_light.wgsl:13 shadow_compare_reference` | metric slack, one home for both consumers |

Two facts about the caster cull that the rest of this design leans on: it is
output-neutral by construction (a caster the cone does not reach contributes
nothing), and the same predicate already exists in WGSL
(`light_index_build.wgsl:126`), bit-identity-tested against the Rust one.

Measured, from `goldens/volumetric-profile-m3-max.json` (stale — §6 step 0
re-baselines it — but the ratios hold):

| case | `caster_draws` | `redrawn_maps` | `unculled_draws` | `cpu_encode` p95 |
|---|---|---|---|---|
| `dense-geometry-noshadow-120` | 0 | 0 | 0 | 4.99 ms |
| `dense-geometry-120` | 3460 | 16 | 32912 | 6.61 ms |
| `zoom-dense-geometry-120` | 3836 | 16 | 32912 | 7.70 ms |

The shadow path's marginal encode cost is `6.61 − 4.99 = 1.62 ms` for 3460
casters over 16 passes: **~0.42 µs per caster**, which is two encoder calls
(`set_bind_group` + `draw_indexed`) at ~0.2 µs each. That arithmetic is the
whole justification for §5's pick.

---

## 2. Storage: three tiers, and why residency ≠ refresh

### 2.1 The tables

```rust
/// Resolution tiers, coarsest last. A fixture's tier is a function of its
/// priority rank, not of its identity, and changes only across the hysteresis
/// margin (§2.3).
const TIER_SIZE:   [u32;   3] = [512, 256, 128];
const TIER_LAYERS: [usize; 3] = [  8,  24,  32];
/// Resident maps. Sum of TIER_LAYERS; a global slot id is an index into the
/// concatenation, so `shadow_slot` stays one number in `LightRest`.
const MAX_FIXTURE_SHADOWS: usize = 64;
/// Maps rendered per frame, across all tiers. The unconditional cost ceiling.
const SHADOW_REFRESH_BUDGET: usize = 12;
/// Of those, at most this many at tier A: a 512² raster is 4x a 256² one.
const TIER_A_REFRESH_BUDGET: usize = 4;
```

Memory, `Rgba16Unorm` at 8 B/texel: 8×512² = 16 MiB, 24×256² = 12 MiB,
32×128² = 4 MiB → **32 MiB** of moment maps, plus ≤14 MiB of prefilter scratch
(§4.3) and ~1.3 MiB of transient depth. Against today's 4 MiB that is a real
jump, and it is the price of 64 residents; the single-resolution alternative
(64 × 512²) is 134 MiB, which is the argument for tiers. Roblox ships 64 MB of
shadow atlas, UE budgets 150 MB.

Layer counts fit `Limits::default().max_texture_array_layers = 256`, so
`Renderer::new`'s `required_limits` (`gpu.rs:888`) needs no change. Metal's
ceiling is 2048 (`wgpu-hal-30.0.1/src/metal/adapter.rs:924`).

### 2.2 The structure that replaces two parallel arrays

`fixture_shadow_slots: [Option<usize>; 16]` and
`fixture_shadow_cache: Vec<Option<ShadowCacheKey>>` are two arrays keyed by the
same slot, updated in two places (`gpu.rs:1988`, `gpu.rs:2617`), and nothing
holds them together. One array of one struct:

```rust
struct Resident {
    cone: usize,            // index into the frame's source-order cone array
    key: ShadowCacheKey,    // what the stored map was rendered from
    aim: Vec3,              // the direction it was rendered with; angular lag is
                            //   measured against this, not against the matrix
    rendered: u64,          // frame counter, for the queue's age term
}

struct FixtureShadows {
    tiers: [ShadowTier; 3],                     // texture + per-layer views + size
    residents: [Option<Resident>; MAX_FIXTURE_SHADOWS],
}
```

`aim` is the one genuinely new field: the angular test (§3.2) needs the
direction the map was built with, and `matrix_bits` cannot answer "how far has
it moved" — only "has it moved". Keep `key` anyway: it carries `caster_hash`,
which `aim` says nothing about.

Slot id → (tier, layer) is `tier_of(slot)` over the fixed prefix sums. The
shaders need the same decomposition; rather than duplicate the arithmetic in two
WGSL files, `LightRest` publishes both — `shadow_slot` (global id, indexes the
matrix array) and `shadow_tier` (0/1/2), the latter taking one of the three
existing `_pad` words (`gpu.rs:2020`), so the struct does not grow and no
packing convention exists in more than one place.

### 2.3 Tier assignment, reusing the hysteresis that is already there

Do not write a second ranking function. `assign_shadow_slots`'s `priority`
closure becomes `pub(crate) fn shadow_priority(cone: &FixtureCone, eye: Vec3) ->
f32`, and `assign_shadow_slots` gains a capacity parameter and is called three
times, tier by tier, each call receiving the candidates the previous tier did
not seat:

```rust
let mut candidates: Vec<usize> = ranked_by_priority(cones, eye);
for tier in 0..3 {
    let seated = assign_shadow_slots(&candidates, eye, previous[tier], TIER_LAYERS[tier]);
    candidates.retain(|c| !seated.contains(c));
}
```

Properties this inherits for free: a resident keeps its tier unless a challenger
beats it by `EVICTION_MARGIN = 1.25` (so a fixture does not oscillate A↔B while
the camera drifts), assignment is deterministic for a given frame, and a cone
that goes dark releases its slot. The only new rule is that a tier change is a
**tenancy change**: the stored map is the wrong resolution *and* possibly
another fixture's content, so it is a must-render (§3.3).

### 2.4 Why not one array and no tiers — design it twice

The alternative is 64 layers at a single 256², 8 MiB of moments, no tier tables,
no per-tier textures, no three-way branch in the shaders. It is meaningfully
simpler and it is the *right* answer if the phase's failing metric is encode
time, which it is.

It loses on one number: quality is not uniform across fixtures, and the fixture
whose shadow edge a designer looks at is the close, wide, bright one — which is
exactly the top of the existing priority ranking, so the information needed to
spend resolution well is already computed. A 512² map at a 15° field is
0.029°/texel against 0.059° at 256²; at a 12 m throw that is 6 mm vs 12 mm of
edge positional error. Tier C is the other half of the argument: dropping the
bottom 32 residents to 128² is what makes 64 residents cost 32 MiB instead of
134 MiB, and 64 residents is the point of the phase.

Honest caveat, recorded so a later measurement can overrule this: tiers are the
*least* measurement-justified item in phase 3. If §5's gates show the encode win
lands and image quality on `fixture-shadowed-beam` is indistinguishable, collapse
to a single 256² tier and keep the residency/refresh split, which is where the
behaviour change actually lives.

---

## 3. Refresh: a priority queue with a hard budget, and a staleness ceiling

### 3.1 The three bands

The single dirty bit (`gpu.rs:2382`) becomes three bands over the angular lag
`Δθ = angle(cone.direction, resident.aim)`:

| band | predicate | meaning |
|---|---|---|
| clean | `Δθ ≤ θ_inv` and `caster_hash` unchanged | the stored map is *correct*; costs nothing |
| stale | `θ_inv < Δθ ≤ θ_max` | wrong but not visibly wrong; the queue decides |
| expired | `Δθ > θ_max`, or tenancy changed | must not be shown |

```
θ_inv = k_inv · field / TIER_SIZE[tier],   k_inv = 2      // texel-scale
θ_max = k_max · field,                     k_max = 0.05   // beam-scale
```

The two thresholds answer different questions and must not be collapsed into
one. `θ_inv` is CryEngine's frustum-relative recentre test applied to
orientation: below two texels of angular displacement the re-render produces
(almost) the same texels, so skipping is free and correct. `θ_max` is
perceptual: the artifact is the shadow edge lagging *its own beam*, so the scale
is the beam's angular width, not the map's texel size. At a 15° field: θ_inv is
0.059° at tier B, θ_max is 0.75°.

A texel-scale ceiling would be far too strict — at the profiler's slew rate
(`profile-volumetrics.rs:684`, `phase = time·1.1`, ≈22°/s ⇒ 0.37°/frame) every
resident would expire within one frame.

### 3.2 The queue

Requests are scored, sorted, and served until the budget is spent:

```
score = shadow_priority(cone, eye) · lag_ratio
lag_ratio = max(Δθ / θ_inv, 1) · (1 + frames_since_request / 16)
```

`shadow_priority` is the same function §2.3 uses — one ranking, two consumers.
The angular factor is self-limiting for a moving fixture (Δθ grows with age
automatically); the explicit age term exists only for `caster_hash` dirtiness,
which has no Δθ and would otherwise starve behind slewing heads forever.

**Expired requests are not privileged.** They enter the same queue at the same
score. What is different is the consequence of losing: an expired resident that
is not served this frame publishes `shadow_slot = -1` for its cone, so the
fixture casts no shadow this frame. That is the phase's central invariant, and
it preserves the reason the count cap was chosen over a throttle in the first
place (`volumetrics-v2.md` Phase 3: *"a shadow that does not line up with the
beam casting it reads as broken, where a fixture that simply casts none reads as
unlit"*). Residency above the refresh budget is opportunistic: it pays when
fixtures hold position, and degrades to today's behaviour when they all slew.

Budget accounting: at most `SHADOW_REFRESH_BUDGET = 12` maps, of which at most
`TIER_A_REFRESH_BUDGET = 4` at 512². Worst-case raster is
4·512² + 8·256² = 1.6 Mpix against today's 16·256² = 1.05 Mpix. A tier-A request
that hits the sub-cap is not demoted mid-frame; it waits, exactly like any other
unserved request.

### 3.3 Tenancy, and the failure the current code cannot have

A slot whose `Resident.cone` changes holds another fixture's depth. Today that
cannot be observed, because a tenancy change always implies a matrix change and
every dirty map redraws unconditionally. Under a budget it can, so tenancy
change is an *expiry*, not a staleness: `Resident` is replaced with
`{ cone, key: <sentinel>, aim: <current>, rendered: 0 }` and the cone's
`shadow_slot` stays −1 until the slot has actually been rendered. Same rule as
today's empty-caster pass, for the same reason (`gpu.rs:2604`): the clear is
what makes a map honest.

### 3.4 What this does to the profiler, predicted in advance

Every cone in the profile scene slews at ≈22°/s, so every resident is dirty
every frame and `θ_inv` saves nothing there. Expect: `redrawn_maps` 16 → 12,
`caster_draws` down proportionally, roughly 25% off the shadow path's marginal
encode, and ~24 of 64 residents holding a valid map (12 served + those within
θ_max's two-frame tolerance) with the remainder publishing −1. **A flat or
slightly worse image on the existing shadow cases is the expected outcome, not a
regression** — the profiler has no static fixtures, and static fixtures are what
residency 64 is for. §5.4 adds the case that exercises it.

### 3.5 Design it twice — the two rejected policies

**Round-robin** is rejected upstream (`volumetrics-v2.md` Phase 3) and the
rejection survives contact with this design: refreshing K of 64 in rotation
gives every fixture a 5-frame lag including the ones that did not move, which
is strictly worse than a queue that spends zero on them. It also cannot express
the staleness ceiling, because it has no per-map notion of how wrong a map is.

**Today's policy — cap residency, always render everything dirty** — is the
serious alternative, and it wins on one axis: it can never show a stale shadow,
so it needs neither `aim`, nor `θ_max`, nor the tenancy rule. It loses because
the cap it needs is a cap on *how many fixtures cast at all*, and 16 of 120 is
visible as missing shadows in a real rig, while 64 of 120 with 12 refreshed is
not — provided the ceiling holds. The whole design is a bet that the ceiling
holds, and §5.4 is how the bet is settled.

### 3.6 What phase 3 deliberately does not do

- **No build-once apex maps.** `volumetrics-v2.md` §7's apex invariant is
  overruled: the emitting aperture is not the pan/tilt pivot, it orbits it, so a
  map cached at a fixed apex is wrong. §15.1's collinearity rescue covers only
  on-axis rays, and the doc's own §15.2 item 5 contradicts its premise for
  multi-emitter heads, whose cells are genuinely off-axis. Every pan/tilt
  invalidates; the lever is the budget, not the cache.
- **No beam-length clamp, no intensity-range culling.** `fixture_shadow_planes`
  (`gpu.rs:4418`) clamps range to 0.05..100 as *sanitisation* — a validity
  bound, nothing scales it by content, and nothing here changes that.
- **No frame-time governor.** `SHADOW_REFRESH_BUDGET` is a constant, not a
  controlled variable. The budget bounds the cost unconditionally, which is the
  property a closed loop would be trying to buy.
- **No static/dynamic caster split.** It follows from the apex veto: with the
  light frustum rotating every frame the cached half is stale every frame.

---

## 4. Representation: 4-moment MSM

### 4.1 Formats, verified against wgpu 30.0.1 in the lockfile

| requirement | answer |
|---|---|
| storage format | `Rgba16Unorm`, 8 B/texel |
| feature gate | `Features::TEXTURE_FORMAT_16BIT_NORM` — `wgpu-types-30.0.1/src/texture/format.rs:737` |
| renderable? | **not** in the guaranteed set: `format.rs:1012` gives `Rgba16Unorm` the `storage` usage alias (`COPY_SRC\|COPY_DST\|TEXTURE_BINDING\|STORAGE_BINDING`), *no* `RENDER_ATTACHMENT` |
| renderable on Metal | yes, with `Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`: `wgpu-hal-30.0.1/src/metal/adapter.rs:211` grants `SAMPLED_LINEAR \| STORAGE_WRITE_ONLY \| COLOR_ATTACHMENT \| COLOR_ATTACHMENT_BLEND \| MULTISAMPLE_X4` |
| both features on Metal | unconditional: `adapter.rs:1188` (`TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`) and `:1192` (`TEXTURE_FORMAT_16BIT_NORM`) |
| filterable sample type | `float { filterable: true }` — `format.rs:1149` |
| WGSL storage format | `rgba16unorm` parses in naga 30 — `front/wgsl/parse/conv.rs:238` |

So `Renderer::new`'s `required_features` (`gpu.rs:879`, today `TIMESTAMP_QUERY`
only) must request **both** features. Requesting only `TEXTURE_FORMAT_16BIT_NORM`
yields a format that can be sampled and stored to but not rendered to — a
validation error at pipeline creation, and the exact trap worth naming here
because the two features look unrelated. `Renderer::new` already returns
`Result`; a device without both is a hard error, not a second code path.

`Rgba16Float` is the degradation if that ever fails (no feature needed,
renderable and filterable everywhere): MSM's quantisation is designed for
*uniform* 16-bit spacing, and fp16's spacing near 1.0 wrecks the fourth moment,
so it needs a larger bias and gives up some of the bleeding headroom. Not the
plan; the fallback if an adapter surprises us.

### 4.2 Raster

The fixture shadow pass stops being depth-only. New pipeline
`fixture_shadow_pipeline`: vertex `vs_fixture_shadow` (§5's caster indirection),
fragment `fs_moments` writing one `Rgba16Unorm` target, depth state unchanged
(`depth_state(true)`, reverse-Z `GreaterEqual`) against a **transient** depth
texture per tier size, attached with the existing `depth_attachment_transient`
(`gpu.rs:4743`, `StoreOp::Discard`) — the depth is scratch that selects the
nearest surface; only the moments are kept.

```wgsl
// z in [-1,1] from the projection's own planes, via the one linearisation
// helper both consumers already share (fixture_light.wgsl:13).
let z = 2.0 * linear01(raw_z, planes.x, planes.y) - 1.0;
let b = vec4<f32>(z, z*z, z*z*z, z*z*z*z);
return quantize(b);   // Peters & Klein optimised 16-bit quantisation
```

**Transcribe the 4×4 quantisation matrix and its offset from Peters & Klein,
I3D 2015 §3.2 / their reference implementation. Do not reconstruct it from
memory** — the constants are non-obvious and a wrong one degrades silently into
bleeding that looks like a bias-tuning problem. The inverse goes in the sampling
path; both belong in `fixture_light.wgsl` so the two consumers share one copy.

### 4.3 Prefilter

One compute pass per frame over the **refreshed layers only** (≤12), separable
5-tap Gaussian, σ = 1 texel: horizontal into a per-tier scratch array sized to
that tier's refresh cap, vertical writing back into the tier layer in place
(safe — the vertical pass reads scratch, not the tier). Bindings: read via
`texture_2d_array<f32>` + `textureLoad`, write via
`texture_storage_2d_array<rgba16unorm, write>`, both available per §4.1. Scratch
cost: 4×512² + 8×256² + 12×128² ≈ 14 MiB.

This is the property that justifies MSM over anything comparison-sampled: the
blur is paid once per light per refresh, and every one of the beam march's
samples reads the already-softened result. PCF cannot amortise across samples at
all.

Mip chains are deliberately not built in phase 3 — there is no LOD selection
story for a shadow lookup from a volume sample, and unused mips are memory plus
a barrier per layer.

### 4.4 Sampling

`scene.wgsl:119` and `beam_transport.wgsl:131` converge on one function in
`fixture_light.wgsl`:

```wgsl
/// Visibility in [0,1] from a prefiltered 4-moment map. One bilinear tap.
fn msm_visibility(b_raw: vec4<f32>, z: f32) -> f32
```

- surface: 9 `textureSampleCompareLevel` → 1 `textureSampleLevel(…, 0.0)`.
- beam: 1 nearest `textureLoad` → 1 `textureSampleLevel(…, 0.0)`, and the
  comment at `beam_transport.wgsl:147` about temporal accumulation supplying the
  soft edge gets deleted along with the behaviour it describes.

Both use explicit-level sampling, so the three-way tier branch is legal in
non-uniform control flow (no implicit derivatives). Binding churn: `group(3)`
bindings 6/7 in `scene_bindings.wgsl:112` become three `texture_2d_array<f32>`
plus one filtering sampler (indices 2 and 3 are free in `cluster_layout`); the
haze layout gains the same (indices 4 and 5 are free — `gpu.rs:2827` uses
0,1,2,3,6,7). `sampler_comparison` disappears from the fixture path entirely and
survives only on the sun cascades.

Starting constants, in the order to tune them:

| knob | start | role |
|---|---|---|
| moment bias α | `3e-5` | `b = mix(b, b_ideal, α)`, `b_ideal = (0, 0.375, 0, 0.375)` — conditions the Hankel solve |
| bleed remap β | `0.3` | `v = saturate((v − β) / (1 − β))` — the standard light-bleeding reduction |
| metric slack | `0.02` (unchanged) | `shadow_compare_reference`'s existing value; MSM keeps it as a precision guard |
| normal offset | `0.006` (unchanged) | `scene.wgsl:127`; surface-only, unrelated to moments |

α and β are the two that get tuned against the goldens. Raising β is the direct
lever on bleeding and it costs contact darkening; raising α costs edge softness.
Record the pair that ships in the shader, next to the constants.

### 4.5 The fallback ladder if bleeding is unacceptable

The bad case is documented and is real for us: a truss element close to the
light with a distant floor behind it, i.e. `occluded-beam` and
`fixture-shadowed-beam`. In order:

1. **Tune β up, α down.** Free. Costs contact darkening, which a beam integral
   hides better than a surface contact does.
2. **Split the representation by consumer:** MSM in `beam_transport.wgsl`, depth
   + 3×3 PCF retained in `scene.wgsl`. Costs a second raster target per map
   (depth *and* moments, +50% memory, one extra store per shadow pass) and
   leaves two shadow representations alive, which is the thing this codebase
   normally refuses. It is nonetheless the right fallback, because the quality
   argument is genuinely consumer-dependent: bleeding integrated along a ray is
   a slight haze lift, bleeding at a surface contact is a visible halo. Note it
   only becomes affordable *because* §5 made a shadow pass cheap.
3. **Abandon MSM, keep everything else.** Tiers, residency/refresh split and
   caster submission are all independent of representation. The beam path then
   gets its soft edge from a 4-tap rotated-disc PCF instead of one `textureLoad`
   — 4× the taps for the thing MSM was going to give for one.

**VSM loses** before it starts: two moments, and its bleeding is worst exactly
on near-occluder/far-receiver pairs, which is our named bad case. **ESM loses**
for a specific reason rather than a general one: its exponent must be tuned
against the depth range, and our per-fixture range varies over the whole
0.05..100 sanitisation band (`gpu.rs:4419`), so no single constant works across
a rig — it would need a per-fixture exponent in the matrix struct and would
still leak through thin occluders. **EVSM** needs 4×fp32 for worse quality than
MSM at 64 bpp.

---

## 5. Caster submission: instance compaction, not proxies

### 5.1 The pick

**Group the shadow pass's draws by mesh and submit one instanced draw per
(map, distinct mesh), with a per-map instance remap table.** Not proxies, and
not §8's full merged index buffer.

Mechanism, entirely CPU-side:

- The existing per-map caster list (`gpu.rs:2502`) is bucketed by `draw.mesh`
  instead of emitted in draw order. The bucketing is a sort of a list that is
  already being built; no new culling, no new predicate.
- The concatenation of all buckets, as `u32` draw indices, is uploaded once per
  frame as a storage buffer (`caster_instances`, ~14 KB at 3460 casters).
- `vs_fixture_shadow` reads `instances[caster_instances[instance_index]]`
  instead of `instances[instance_index]`. The offset comes free from
  `first_instance`: WebGPU defines `@builtin(instance_index)` as
  `first_instance + i`, and the existing call already passes a non-zero range
  (`gpu.rs:2578`, `i..i+1`).
- The pass draws `draw_indexed(first..last, base, off..off+count)` once per mesh
  bucket.

Second, independent saving in the same step: the fixture shadow pass gets its
own bind group layout (shadow globals + instances + `caster_instances`) instead
of borrowing `scene_pipeline_layout`. That deletes the per-draw
`set_bind_group(1, &materials[i])` in `draw_range` (`gpu.rs:2575`) — the shadow
pass binds materials it never reads, at one encoder call per caster.

Expected: `dense-geometry-120`'s 3460 caster draws become 16 maps × ~17 distinct
meshes ≈ 272 draws, and the two encoder calls per caster become one per bucket.
At the measured 0.42 µs/caster that is ~1.5 ms off `cpu_encode_p95`, which is
the whole gap to the 4.0 ms budget. Combined with §3's budget (16 → 12 maps) it
should land near 3.0 ms.

**Falsification, stated up front:** if removing ~3200 draw calls does not move
`cpu_encode_p95` by ≥1 ms, the cost is not per-draw submission — it is pass
setup, bind-group creation or per-frame buffer churn — and the next step is the
profiler's encode breakdown, not more submission work.

### 5.2 Why not caster proxies

Killzone's 60–80% triangle reduction attacks GPU vertex cost. Ours is not the
binding constraint: `dense-geometry-120` shows `gpu_total` p95 1.98 ms for the
*entire frame* including 16 shadow passes, against `cpu_encode_p95` 4.6–4.9 ms
failing a 4.0 ms budget. Proxies remove **zero** draw calls, so they move the
failing number not at all. They also cost an asset pipeline, a second mesh
identity per model (change amplification through `assets`, `frame::Draw`,
`mesh_bounds`, the golden scenes) and a new authoring obligation for every venue
model. Right idea, wrong phase: revisit when 512 shadowed cones makes the shadow
raster GPU-bound.

### 5.3 Why not the full merged index buffer (§8)

id's merge exists because they cannot group by mesh — *"up to 15k instantiated
models"*, all distinct. Our dense case is 120 copies of ~17 meshes, so grouping
by mesh already collapses 216 draws per map to ~17, and the merge's remaining
win over that is the last ~17 calls. Against that: it needs the vertex buffer
re-bound as `STORAGE` and read manually (stride 48, `gpu.rs:1102`, positions at
offset 0), a compute pass writing packed `VertexID | InstanceID` indices, an
arena with a spill path when it overflows, GPU-side caster culling (moving
`ShadowStats::caster_draws` to a one-frame-late readback), and a bit-budget
decision (20 bits of vertex id / 12 bits of instance) that becomes a silent
correctness cliff on a large venue.

**Escalation trigger, so this is a decision and not a preference:** if a real
venue's `caster_draws / distinct_meshes_per_map` ratio drops below ~2 — many
distinct meshes rather than many copies — mesh grouping stops paying and §8 is
the answer. Add `distinct_meshes_per_map` to `ShadowStats` in this step so the
trigger is measured rather than guessed.

---

## 6. Migration, at file granularity

Every step compiles, and every step states its gate. Two steps re-baseline
goldens; each does so for exactly one attributable reason.

**Step 0 — re-baseline the profile golden.** `goldens/volumetric-profile-m3-max.json`
records `budgets_ms.mean_lights_per_cluster` while the code emits
`mean_lights_per_tile` (`bin/profile-volumetrics.rs:620`), so it predates the
LightIndex cutover. Without this there is no "before".

**Step 1 — extract, no behaviour change.** New
`gpui/crates/render/src/shadow.rs`: `FIXTURE_SHADOW_SIZE`, `MAX_FIXTURE_SHADOWS`,
`ShadowCacheKey`, `shadow_matrix_bits`, `fixture_shadow_caster_hash`,
`fixture_shadow_planes`, `fixture_shadow_matrix`, `assign_shadow_slots`,
`fixture_shadow_texture_array`, and the `Resident` array behind a
`FixtureShadows` struct with methods for the four sites `gpu.rs` touches today
(1988, 2379, 2502, 2617). `gpu.rs` keeps the encoding. Move the existing unit
tests (`gpu.rs:3336`, 3412–3520) with them.
*Gate: contract goldens bit-identical; profiler unchanged.*
*Landed 2026-08-25 (partial): consts, `ShadowCacheKey`, the free functions, and
both slot tests moved to `shadow.rs`; gate held bit-identical. The
`FixtureShadows` struct facade (residency array + the four call sites) is
deferred to the full phase-3 pass — extracting it now would only add a
pass-through layer until steps 2–4 give it real behaviour to own.*

**Step 2 — tier tables with one tier.** `TIER_SIZE = [256, 256, 256]`,
`TIER_LAYERS = [16, 0, 0]`. `LightRest.shadow_tier` added and always 0; shaders
gain the three-way branch with two dead arms. Exercises the whole plumbing
against an identity configuration.
*Gate: bit-identical.*

**Step 3 — three real tiers, residency 64.** Flip the constants; add the
per-tier `assign_shadow_slots` cascade (§2.3). Refresh still unbudgeted (every
dirty resident renders), so the only behaviour change is resolution and
residency. **Re-baseline #1**, reason: golden fixtures rank into tier A and
their maps get sharper. Verify the delta is confined to shadow edges and that
`renderer_contract_goldens.rs`'s invariants still hold
(`fixture_shadow_pixels > 500`, `mean_rgb(shadowed) < mean_rgb(open)`).

**Step 4 — the queue.** `Resident.aim`, θ_inv / θ_max, the score, the budget,
the tenancy rule, `shadow_slot = -1` on unserved expiry. `ShadowStats` gains
`residents`, `refreshed`, `expired_unserved`, `distinct_meshes_per_map`.
*Gate: bit-identical — the goldens are single-frame captures of scenes with
fewer casters than the budget, so the throttle is invisible to them. That it is
invisible is itself worth asserting.*

**Step 5 — caster submission (§5).** New `fixture_shadow_pipeline` +
`vs_fixture_shadow` + its bind group layout; `caster_instances` buffer; mesh
bucketing in `shadow_casters`; the fixture pass stops calling `draw_range`. Sun
cascades keep `shadow_pipeline` / `vs_depth` untouched.
*Gate: bit-identical. Same geometry, same depth test, same clear; a coplanar tie
resolved differently by draw order is the one thing that could move a pixel, and
if one moves, that is what to look for. This is the step that must move
`cpu_encode_p95`.*

**Step 6 — MSM.** `Rgba16Unorm` tier textures + transient depth; the two device
features in `Renderer::new`; `fs_moments`; the prefilter compute pass and its
scratch arrays; `msm_visibility` + quantisation in `fixture_light.wgsl`; both
consumers' sampling and both bind group layouts. **Re-baseline #2**, reason:
every shadowed pixel changes by construction. New assertion in
`renderer_contract_goldens.rs`: the in-shadow mean of `fixture-shadowed-beam`
must not exceed the recorded PCF baseline by more than 15% — the bleeding gate,
cheap and specific.

**Step 7 — delete.** `hard_shadow_sampler` / `shadow_sampler` keep serving the
cascades; `dummy_shadow` (`gpu.rs:1381`) becomes a 1×1 `Rgba16Unorm` array for
the fixture path and stays depth for the cascade path. Sweep
`FIXTURE_SHADOW_SIZE`'s remaining uses (`gpu.rs:2270`, `:2745` — the
`1.0/size` texel constant is a PCF artifact and dies with the 3×3 loop).

---

## 7. Measurement plan

Run `profile-volumetrics --release`, re-baselined at step 0.

| case | metric | today | gate |
|---|---|---|---|
| `dense-geometry-120` | `cpu_encode_submit` p95 | 4.6–4.9 ms | **< 4.0 ms** after step 5, target ~3.0 |
| `zoom-dense-geometry-120` | `cpu_encode_submit` p95 | 4.6–4.9 ms | **< 4.0 ms** after step 5 |
| `dense-geometry-120` | `caster_draws` | 3460 | **< 400** after step 5 |
| `fixture-shadows-120` | `gpu_total` p95 | 1.73 ms | must not regress past 2.5 ms at step 3 (tier A raster), back under 2.0 after step 6 |
| `fixture-shadows-120` | `redrawn_maps` | 16 | **≤ 12** after step 4 |
| all shadowed | shadow raster Mpix/frame | 1.05 | **≤ 1.6**, new stat |
| `mixed-slew-shadows-120` (new) | `residents` with a valid map | n/a (16 today) | **≥ 48** |
| all | fixture shadow memory | 4 MiB | ≤ 48 MiB, recorded not gated |

**New profiler case, and it is required, not optional.** Every existing case
slews every cone at ≈22°/s, which is the worst case for the refresh budget and
the best case for making it look useless (§3.4). Add `mixed-slew-shadows-120`:
same rig, but 70% of cones hold a fixed direction and 30% slew. That is the
distribution a real cue has, it is the only case where residency 64 can show a
win, and without it step 4 has no defensible before/after.

Also worth adding while in the file: a per-case `shadow_expired_unserved` count.
If it is non-zero outside the all-slewing cases, either the budget or θ_max is
wrong, and the number says which.

---

## 8. Risks

- **The staleness ceiling is the whole bet.** If θ_max = 0.05·field turns out to
  be visible, the fallback is to lower it, which converges on today's behaviour
  with extra machinery. Settle it on `mixed-slew-shadows-120` with a side-by-side
  against `SHADOW_REFRESH_BUDGET = 64` (effectively unthrottled) before tuning
  anything else.
- **MSM bleeding on `occluded-beam`.** §4.5 is the ladder; step 6 is the last
  step precisely so that abandoning MSM costs nothing already landed.
- **One transient depth texture shared across a tier's passes** serialises those
  passes on that resource. If the GPU timeline shows the shadow passes losing
  overlap, give tier A one depth texture per concurrent map (4 MiB).
- **Two features added to `required_features`.** Any adapter without them now
  fails `Renderer::new`. Metal grants both unconditionally in wgpu 30, and this
  renderer is macOS-only in practice, but the error message should name the
  features rather than surfacing a wgpu validation string.
- **`shadow_tier` occupying a `_pad` word** couples `LightRest`'s layout to the
  tier scheme in three files. `const_assert` the struct size and keep the
  encoding in one Rust function and one WGSL function.
- **Golden re-baselines are where a real regression can hide.** Both are
  attributable to one mechanism by construction (step 3: resolution; step 6:
  representation). If a re-baseline shows a delta that mechanism does not
  explain — a moved silhouette, a shifted highlight — stop and find it.

## 9. Open questions

- Should tier C (128²) sample *without* the prefilter blur? At 128² a 5-tap σ=1
  Gaussian is a large fraction of the map and may erase the shadow entirely.
  Likely wants σ scaled to tier, measured on `fixture-shadowed-beam`.
- Do the sun cascades eventually want MSM too? They are 3 cached maps of the
  whole scene, and their 3×3 PCF is not on any profile's critical path. Leaving
  them on depth is the right call for phase 3, but two shadow representations in
  one renderer is a standing smell to re-examine once MSM's constants are tuned.
- `FixtureCone` still has no id; residency identity is "position in the frame's
  cone array" (`light-index-unification.md` §3 flags the same contract). A
  `FixtureId` newtype would make tenancy changes exact rather than inferred.
  Out of scope, and it gets more load-bearing with every phase.
