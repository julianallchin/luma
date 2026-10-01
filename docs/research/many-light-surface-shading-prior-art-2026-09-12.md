# Many-light surface shading and shadows: prior art and experiments

Date: 2026-09-12  
Target: Luma's native `wgpu` renderer on an M3 Max (40-core GPU)  
Scope: opaque surface lighting and fixture shadows with haze off

Measured follow-up: [Gasworks surface experiments, 2026-09-14](gasworks-surface-experiments-2026-09-14.md).
That saved rig has 310 cones but only 10 face lights. The omissions isolate
the fixture surface-lighting path as expensive; none of the three tested
output-preserving prototypes earned a default renderer change.

## Finding

The retained sample (UI 1.2 ms, CPU encode 2.13 ms, GPU total 13.59 ms,
light-index build 0.38 ms, draw 20.3 ms, zero shadow maps redrawn) establishes a
GPU-heavy frame, but it does **not** identify which part of the scene shader is
responsible. `gpu_total_ms` contains more than fixture surface shading, and a
single frame does not provide stable attribution. The next step should be the
already-supported shader ablation matrix below, not a renderer rewrite.

There is one strong source-level suspect that the existing many-light design
documents do not emphasize: fixture **face point lights are not indexed**. The
surface shader loops over every active face light for every visible fragment,
computes a distance, then rejects almost all of them against a very small
cutoff ([`scene.wgsl`](../../gpui/crates/render/src/shaders/scene.wgsl), lines
476-492). The frame builder creates one such point light for every lit
beam-capable fixture ([`frame.rs`](../../gpui/crates/render/src/frame.rs), lines
953-962). In an `ALL` cue this can be hundreds of wasted tests per pixel. By
contrast, fixture cones already use a sophisticated tiled/depth-filtered path.

If the face-light ablation is large, index those small spheres before doing
more work on cone culling or shadows. This is exact relative to the current
renderer and is standard Forward+ practice. Apple's own Forward+ sample uses a
depth prepass, tile light culling, and a per-tile light list—the same broad
shape Luma already uses for cones ([Apple Forward+ sample](https://developer.apple.com/documentation/metal/rendering-a-scene-with-forward-plus-lighting-using-tile-shaders?changes=_8&language=objc),
[WWDC19](https://developer.apple.com/videos/play/wwdc2019/601/)).

## What is already implemented

The current tree was checked rather than inferred from older design notes.

- Fixture cones use 8 px screen tiles, fixed 512-bit masks, 4,096 view-depth
  bins, depth-sorted light records, and a cone-versus-tile-wedge narrow phase
  ([`light_index.rs`](../../gpui/crates/render/src/light_index.rs)). This is
  already the core of clustered forward shading described by Olsson, Billeter,
  and Assarsson ([HPG 2012 paper](https://www.cse.chalmers.se/~uffe/clustered_shading_preprint.pdf)).
- At 32 or more on-screen cones, a 4x-MSAA surface-depth prepass refines the
  masks using all covered depths. Tiles with large depth variance split into
  two buckets. This directly addresses the false positives caused by long tile
  frusta. Wronski's recommended cone/sphere test is already present
  ([author's analysis](https://bartwronski.com/2017/04/13/cull-that-cone/)).
- Every active fixture can retain a 256-square shadow map, with capacity grown
  to 512 layers across two arrays. Cache identity is the light projection plus
  caster geometry; camera motion is absent from the key. With zero maps redrawn,
  shadow rasterization, caster submission, refresh budgets, and cache
  invalidation are not the cost in the reported orbiting-camera frame
  ([`gpu.rs`](../../gpui/crates/render/src/gpu.rs), lines 5264-5315 and
  5823-6240).
- The surface shadow lookup is expensive by design. It reconstructs the
  receiver-plane depth gradient, gathers sixteen unique texels in four texture
  operations, performs sixteen individually corrected comparisons, and
  combines the exact 3x3 bilinear-PCF weights
  ([`scene.wgsl`](../../gpui/crates/render/src/shaders/scene.wgsl), lines
  137-241). A nested-loop version is documented as 1.9 ms slower on the M3 Max.
- A min/max hierarchy is already built for every dirty shadow map and retained
  with the cache, although only volumetric transport consumes it today
  ([`shadow_hierarchy.rs`](../../gpui/crates/render/src/shadow_hierarchy.rs)).
- Two precomputed gobo invariants, `inverse_right_length` and `field_tangent`,
  are uploaded per light and used by the volumetric shaders, but the surface
  path calls the shared helper that recomputes normalization and field-angle
  terms. This is a small exact cleanup worth measuring after attribution.

## Ranked options

"Exact" below means preserving the current raster result, aside from ordinary
floating-point reassociation. It does not mean the current 256-square shadow
map is physically exact.

| Rank | Technique | Quality class | Why it fits this case |
|---:|---|---|---|
| 1 | Put face point lights behind conservative tiled/depth culling | Exact | Removes a currently unbounded `pixels × active face lights` loop. Small cutoff spheres should produce sparse masks. Confirm first with `LUMA_PROFILE_OMIT=face-lights`. A fixture-local implementation could be simpler and matches the type's stated intent, but needs a contract test because it can differ from today's incidental lighting of nearby geometry. |
| 2 | Use the existing shadow min/max hierarchy for conservative all-lit/all-shadowed early exits before the full PCF filter | Exact if receiver-depth bounds include every tap | The hierarchy is already resident and valid when maps are cached. Large uniform regions can avoid four gathers, sixteen metric depth conversions/comparisons, and weight accumulation. A conservative miss falls through to the current code. |
| 3 | Remove avoidable per-candidate arithmetic and divergence | Exact | Compare squared distance with squared range before `sqrt`; use the uploaded gobo basis invariants; specialize or bucket plain washes and gobo lights if Xcode shows divergent transcendental work. These changes are small and independently testable. |
| 4 | Sweep the current exact culler rather than replace it: 4/8/16 px tiles, one/two surface-depth buckets, and tighter rasterized cone coverage only if candidate counters remain high | Exact | The 0.38 ms index time leaves some room to spend on culling if it saves several milliseconds of shading. Tiled-light-tree work shows that hierarchical traversal helps worst cases but can be slower than ordinary clustering, motivating a hybrid only after occupancy proves the need ([O'Donnell and Chajdas 2017](https://anteru.net/research/tiled-light-trees/)). |
| 5 | Build an Apple-specific tile-shader Forward+ or single-pass deferred path | Exact within current material/shadow semantics; platform-specific | Apple GPUs can keep light lists and G-buffer data in imageblock/threadgroup tile memory and avoid device-memory round trips ([Apple TBDR guidance](https://developer.apple.com/documentation/metal/tailor-your-apps-for-apple-gpus-and-tile-based-deferred-rendering), [Apple deferred sample](https://developer.apple.com/documentation/Metal/rendering-a-scene-with-deferred-lighting-in-swift)). Luma already has the depth prepass Forward+ needs. However, `wgpu = 30` exposes no tile-shader/imageblock render API, so this requires a Metal-specific escape hatch or backend. A conventional multi-pass deferred implementation through portable `wgpu` writes and rereads a G-buffer, exactly the bandwidth cost Apple warns about. |
| 6 | Replace the receiver-plane filter with hardware comparison PCF / optimized bilinear PCF | Approximate here | WGSL can compare and bilinearly filter four depth texels per `textureSampleCompareLevel`, and can return four comparisons via `textureGatherCompare` ([WGSL texture operations](https://www.w3.org/TR/WGSL/#texture-builtin-functions)). Both accept one depth reference for the footprint. Luma deliberately uses a different receiver-plane-corrected reference for each of sixteen texels, so the hardware form cannot reproduce the current answer. It may still be a useful quality/performance mode if an orbiting-camera golden shows no grazing acne. |
| 7 | Shadow-map resolution tiers, moment shadow maps, or virtual shadow pages | Approximate; low priority for this sample | Lower-resolution tiers improve cache locality but soften and move shadow edges. Moment Shadow Mapping reduces lookup to one filtered 64-bit sample but computes a lower-bound approximation and can bleed ([Peters and Klein 2015](https://momentsingraphics.de/I3D2015.html)). Virtual shadow maps allocate, render, and cache only needed pages and select LOD by projected pixel size ([Epic documentation](https://dev.epicgames.com/documentation/unreal-engine/virtual-shadow-maps-in-unreal-engine)), but Luma's maps are already only 256 square, fully cached, and not redrawing in this case; page tables add complexity without an established construction or memory problem. |
| 8 | Deterministic update budgeting / stale-shadow reuse | Approximate during moving-light or geometry edits; no steady-state win | Useful for redraw storms, and honest only with a maximum staleness rule. It cannot improve a frame reporting zero redrawn maps. Light-space caching already gives exact visibility reuse across camera motion. |
| 9 | Sample lights stochastically with reservoirs, then spatially/temporally reconstruct | Approximate and temporally sensitive | ReSTIR resamples a few candidates across neighboring pixels and frames and reports large equal-error speedups, but its output is Monte Carlo noise plus reconstruction; the biased form trades more speed for energy loss ([Bitterli et al. 2020](https://research.nvidia.com/labs/rtr/publication/bitterli2020spatiotemporal/)). A 2025 variant selects full-resolution shadow maps with ReSTIR and uses imperfect maps for the remainder ([Zhang et al. 2025](https://research.nvidia.com/labs/rtr/publication/zhang2025many-light/)). This is attractive only as a dense-rig fallback: camera orbit, quick pan/tilt, gobos, and lighting cues expose boiling, lag, and disocclusion noise precisely where a previz tool needs immediate fidelity. |
| 10 | Lightcuts, light BVHs, neural visibility, or other light hierarchies | Approximate unless traversed to leaves | Lightcuts gives bounded-error group approximations with strongly sublinear cost ([Walter et al. 2005](https://www.cs.cornell.edu/~kb/projects/lightcuts/)); dynamic light BVHs are primarily importance samplers for stochastic ray tracing. A hierarchy traversed to every contributing leaf preserves Luma's result but saves little after the tile index. Grouping distinct moving spotlights or procedural gobos weakens bounds and risks visibly merging the authored look. |

## The shadow-filter opportunity

The most plausible exact shadow experiment is not a new representation. It is
a conservative fast path over Luma's existing min/max hierarchy:

1. Project the receiver and compute the same plane gradient as today.
2. Bound the minimum and maximum corrected receiver reference over the 4x4
   texel footprint.
3. Read a conservative min/max caster-depth node covering that footprint.
4. Return 1 or 0 only when the bounds prove that all sixteen comparisons agree;
   otherwise execute the existing filter unchanged.

This follows the general min/max hierarchical-shadow approach used to classify
lit, shadowed, and unresolved regions before falling back to PCF (for example,
the min-max hierarchy in [Variable Soft Shadow Mapping](https://jankautz.com/publications/VSSM_PG2010.pdf)).
It is output-preserving only if the hierarchy node fully covers the footprint,
reverse-Z inequalities are correct, the 0.02 m metric slack is included, and
the receiver reference is bounded at every tap. The fast path should count
`all_lit`, `all_shadowed`, and `fallback` outcomes. If fallback dominates, remove
the experiment; the extra hierarchy fetch and branch can make the shader worse.

Moment maps are the next filter experiment only if the ablation proves shadow
sampling is dominant and the exact hierarchy fast path fails. They exchange
four depth gathers for a larger filtered moment texture plus reconstruction
math, and their light bleeding is especially risky for a truss close to a
receiver. A still-image global error average is insufficient; inspect moving
shadow edges and near/far occluder pairs.

## Apple GPU and `wgpu` boundary

Apple's architecture changes the forward/deferred tradeoff. Its TBDR removes
hidden fragments before shading and keeps render targets in fast tile memory.
Apple recommends coalescing work into fewer render passes, and its native tile
shaders can retain culled light lists in persistent threadgroup memory
([TBDR guidance](https://developer.apple.com/documentation/metal/tailor-your-apps-for-apple-gpus-and-tile-based-deferred-rendering),
[WWDC20 Apple-silicon guidance](https://developer.apple.com/videos/play/wwdc2020/10632/)).
This favors Luma's clustered forward structure more than a portable,
multi-pass G-buffer conversion. The 4x-MSAA path also aligns with clustered
forward's established advantage over tiled deferred when MSAA is enabled
([Olsson et al. 2012](https://doi.org/10.1145/2343045.2343095)).

The native Metal path is richer than portable `wgpu` here. Apple exposes tile
shaders, imageblocks, raster-order groups, sparse textures, and hardware ray
tracing ([Metal feature tables](https://developer.apple.com/metal/feature-sets/)).
Current `wgpu` documentation lists ray queries as experimental,
native-only, and Vulkan-only; they are not available on its Metal backend
([`wgpu` feature documentation](https://docs.rs/wgpu/latest/wgpu/struct.FeaturesWGPU.html#associatedconstant.EXPERIMENTAL_RAY_QUERY)).
Therefore ReSTIR DI with hardware visibility rays is not a near-term portable
Luma option, despite M3 hardware support. Raster-shadow-map reservoir sampling
is possible in WGSL, but it retains the temporal/noise tradeoff and still needs
the maps.

## Measurement plan

Use one deterministic Gasworks camera orbit, the same quarter-screen viewport,
playback stopped, haze off, `ALL` held constant, mains power, and no concurrent
build. Record at least 60 warm-up and 600 measured frames. Report p50/p95/p99,
not one retained sample.

### 1. Attribute the current scene shader

Run the baseline and one process restart for each existing specialization:

```text
LUMA_PROFILE_OMIT=surface-lighting
LUMA_PROFILE_OMIT=surface-shadows
LUMA_PROFILE_OMIT=face-lights
```

Use `gpu_scene_ms` when `redrawn_shadow_maps == 0`; also retain `gpu_total_ms`,
viewport pixels, lit cone count, active face-light count, refined candidate
visits, and actual shadow-filter calls. `surface-lighting` removes the fixture
cone loop; `surface-shadows` preserves cone PBR but returns visibility 1;
`face-lights` isolates the unindexed point-light loop. Differences between
separate runs are attribution signals, not additive accounting, because
removing work changes occupancy and overlap.

Then capture the slow baseline frame in Xcode's Metal debugger. Apple's tools
provide per-line shader costs and counters for occupancy, bandwidth, fragments,
and texture-filter utilization ([Apple GPU profiling guide](https://developer.apple.com/documentation/xcode/optimizing-gpu-performance),
[counter-statistics guide](https://developer.apple.com/documentation/xcode/analyzing-apple-gpu-performance-using-counter-statistics)).
This distinguishes transcendental/ALU pressure from shadow-texture latency and
register-limited occupancy.

### 2. Test scaling laws

- Sweep active face lights while keeping fixture cones fixed. A linear slope in
  face-light count, removed by the face-light ablation, licenses indexing them.
- Sweep viewport area at fixed camera and light state. Per-fragment shading
  should scale with shaded pixels; an index-build problem scales with tile count.
- Run `LUMA_SURFACE_DEPTH_CULL=0` against the default and collect **refined**
  candidate visits. Existing `fragment_stats()` counts the original mask/Z-bin
  candidates, not the two refined surface planes, so it cannot evaluate this
  optimization by itself.
- If cone visits remain high, prototype 4 px tiles and record index time,
  refined visits, scene time, and mask memory together. Keep the version only
  if total GPU time falls; a shorter list is not an outcome by itself.

### 3. Gate each prototype on fidelity and latency

For exact paths, require byte-identical renderer contract goldens where floating
reassociation does not intervene, plus the existing subpixel/coplanar receiver
tests. For approximate paths, compare against the current exhaustive filter on:

- a scripted orbit at normal operator speed;
- fast pan/tilt and blackout/restore cues with no warm-up history;
- narrow gobos across textured and glossy receivers;
- a truss close to a floor, where moment-map bleeding and receiver-plane acne
  are easiest to see;
- p95 input-to-present latency, not only settled image error.

Do not accept a technique solely from a converged still. Previz quality is
defined by the first frames after camera and lighting changes.

## Recommended order

1. Run the three existing shader ablations and an Xcode shader-cost capture.
2. If face lights are material, index their cutoff spheres or encode their
   fixture-local contract; remeasure before touching fixture cones.
3. If fixture surface shadows are material, prototype and instrument the exact
   hierarchy early-out.
4. Apply the small invariant/squared-distance cleanups only where the shader
   profiler confirms cost.
5. Sweep culling granularity only if refined candidate counts remain high.
6. Consider hardware PCF or moment-map modes only after exact options fail the
   frame budget and moving-edge review establishes an acceptable trade.
7. Treat Metal tile shaders and stochastic many-light rendering as separate
   architecture projects, not incremental fixes for this un-attributed frame.

The adjacent code smell is the split many-light policy: cone emitters have a
four-plane clustered index, while their face lights use an all-pixels/all-lights
loop. That asymmetry should be resolved if the ablation confirms it. The unused
surface copies of the two gobo invariants are a smaller duplicated-work smell;
they should not displace source attribution.
