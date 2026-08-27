# LightIndex — unifying the two light cullers

Companion to `volumetrics-v2.md` §1.3(c), §3.2 and Phase 4. Scope: replace
`clusters.rs` and `gpu.rs::haze_tiles` with one module, one bounds math, one
structure, serving both consumers. No code changes here — this is the design
that Phase 4 implements.

---

## 0. Recommendation

**Go straight to the §3.2 structure — tiles + Z-bins, fixed-width masks, no
cache — and land it in one change. Do not build a CPU-side unification first.**

Split the build by cost class rather than by "CPU now, GPU later":

| work | where | why |
|---|---|---|
| sanitise, tight cone sphere, screen rect, view-depth span | CPU, `O(lights)` | 512 elements; already done there; it is not the cost |
| sort lights by view depth, reorder the SoA, fill the Z-bin LUT | CPU, `O(lights)` | a 512-element sort is ~10 µs; a GPU sort is a subsystem |
| assign lights to 32,400 tiles | **GPU compute**, `O(lights × tiles)` | this is the entire 4–14 ms, and it is embarrassingly parallel |

The measured cost is not spread across the builder; it is one nested loop.
Moving that loop and leaving the rest where it is gets the whole win at a
fraction of the Phase 4 surface area, and it keeps the CPU half readable and
directly testable.

Three claims justify skipping the CPU-only intermediate:

1. **A CPU unification cannot fix what is broken.** Merging the two builders
   leaves `cpu_cluster` at 4–5 ms (beams-at-camera-128, rebuilding 720/720
   frames) and 13.8 ms cold at 512 cones. It would make the CPU builder do
   strictly more work, since it must now emit both consumers' output.
2. **The intermediate artefact is the throwaway one.** "Extend `clusters.rs` to
   also emit 2D tile lists, by projecting or maxing over Z" produces a second
   CSR structure that Phase 4 deletes wholesale. Whereas in the tiles+Z-bins
   structure the 2D list *is* the tile mask — the ray consumer reads the same
   bytes the point consumer reads, with the Z-bin refinement skipped. Serving
   haze costs zero additional storage and zero additional build work. That is
   the decisive property, and it is the one the current pairing lacks.
3. **The cache is the unsoundness, and a CPU merge would have to merge two
   unsound keys into one.** Wasted design effort on a mechanism that is being
   deleted (§6).

wgpu 30 is already the pinned dependency (`crates/render/Cargo.toml: wgpu =
"30"`), so nothing gates the compute path.

### Is the haze-side unification throwaway, given BeamPass Phase 2?

Partly — and it does not matter, because the part that is throwaway costs
nothing.

Under the endpoint architecture (§3.0/§3.1/§3.4) the ray consumer genuinely
disappears: beams become rasterized cone proxies with no tile list, and the
ambient bed becomes a froxel grid. But note what the froxel grid needs — a
per-froxel light set, which is a *3D* consumer of exactly this index, tile mask
∩ Z-bin range, identical to the surface consumer. So the endpoint has two 3D
consumers and zero 2D consumers, and LightIndex outlives BeamPass either way.

The 2D consumer is served by `mask` with no Z restriction: one line in the
shared WGSL prelude. When BeamPass lands it deletes that line and the haze
binding, and the module is unchanged. There is no throwaway work to weigh.

### Ordering against BeamPass

**LightIndex should land first.** BeamPass is the larger and riskier change —
per the adversarial review it has an unbounded worst case that is precisely
`beams-at-camera-128`, plus eye-inside batching, adaptive sample counts, a
depth-aware upsample and a golden re-baseline. LightIndex is smaller, its
correctness gate is sharp (§10), and it removes a class of bug rather than
trading one cost model for another. Landing it first also gives BeamPass a
clean before/after number on a culler that is no longer a confound.

---

## 1. What is actually divergent today, precisely

Worth stating because two of the four divergences are narrower than they look,
and one is worse.

- **Tile size.** `CLUSTER_TILE_SIZE = 32` at full res; `HAZE_TILE_SIZE = 16` on
  a buffer at `haze_resolution` ∈ 0.25..1.0. At the shipping 0.5 the two grids
  are *the same 32 full-res pixels and the same column and row counts*. They
  diverge only when the quality slider moves — which is the real smell: **the
  culling granularity is a function of one consumer's render resolution.** The
  unified index is defined in full-resolution pixel space and consumers scale
  their fragment coordinate. That divergence becomes unrepresentable.
- **Bounds math.** `clusters.rs::bounds_for` builds a per-axis radial extent
  around the cap disc (correct, reasonably tight). `haze_tiles` uses
  `Vec3::splat(radius)` — a cube around the cap centre, roughly `√3`× the radial
  extent in the worst axis. This is the real defect and it has no defence.
- **Near-plane clipping.** Already shared (`box_corners`,
  `for_each_clipped_vertex`) since Phase 1. Both `behind_eye ⇒ whole screen`
  branches are gone. This one is done.
- **Cache keys.** `ClusterCacheKey` quantises the camera by
  `MOTION_QUANTUM_TILES`; `HazeTileKey` uses exact camera bits. Both hash cone
  topology with the same FNV walk over the same nine floats — the same function,
  written twice (`clusters::topology_hash` and `gpu::haze_tile_key`). Both are
  unsound for a static camera with moving lights; the haze one additionally
  rebuilds on every sub-pixel camera drift.

A third consumer already shares the bounds vocabulary and should keep doing so:
shadow caster culling calls `clusters::cone_reaches_sphere` directly
(`gpu.rs:2685`) without touching the grid. That is the right shape — the
geometric predicate is a primitive, the index is a structure built from it — and
LightIndex must preserve it as a `pub(crate)` free function, not absorb it.

---

## 2. The structure

Drobot's tiles + Z-bins, with the §3.2 parameters. Restating them here with the
arithmetic for our ceilings, because two of them are load-bearing.

| parameter | value | at 1920×1080 |
|---|---|---|
| tile size | 8 px, full-res space | 240 × 135 = 32,400 tiles |
| per-tile mask | 512 bits = 16 × u32 = 64 B | **2.07 MB, fixed** |
| Z-bins | 4096, uniform in view depth, `min\|max` packed u32 | 16 KB |
| light ceiling | `MAX_FIXTURE_CONES = 512` | exactly one mask width |

The mask width and the fixture ceiling being the same 512 is not a coincidence
worth relying on silently — `light_index.rs` should `const_assert` that
`MAX_FIXTURE_CONES == MASK_WORDS * 32`, so raising one without widening the
other is a build error rather than a silent truncation.

For scale against what it replaces: `transport-512` currently packs 5,793,674
CSR references (23.2 MB) plus 261 KB of headers, and `transport-128` packs
1,697,114 (6.8 MB). The mask is 2.07 MB **regardless of occupancy or cone
count**, which is the property that matters — no allocation, no growth path, no
`max_lights_per_cluster` cliff, and `Renderer::storage`'s per-frame
`create_buffer_init` has nothing left to allocate here.

**Why a mask and not a wider CSR.** Phase 1's slice sweep is the argument and it
is already measured: raising `CLUSTER_DEPTH_SLICES` 16 → 256 improved mean
lights/cluster 121.6 → 55.0 but *inflated* references 263k → 574k, because a
finer grid means more occupied cells means more entries. A fixed-width mask
decouples depth resolution from storage entirely, which is what lets us go to
4096 bins.

**Why Z-bins and not more slices.** The Z-bin is a `min|max` light-id range over
a depth slab, valid only because lights are globally sorted by view depth. Cost
is 4 bytes per bin *independent of how many lights occupy it*. A fragment
computes `range = zbin[slice]`, then walks only the mask words spanning
`[min>>5, max>>5]` with edge masks on the first and last word — Drobot's
word-range loop. At 4096 bins the slab is thin enough that the range is tight
in practice, and the depth test is exact by construction rather than a
16-splinter approximation.

**Why the point/ray split falls out for free.** The point consumer
(`scene.wgsl`, and later froxel injection) intersects mask with the Z-bin range.
The ray consumer (`haze.wgsl`) skips the intersection and walks the mask. Same
buffer, same words, no second structure. Compare the alternative under a CSR
grid, where the ray consumer must either walk 16 clusters per pixel or be given
its own projected copy — which is the situation we are in today, and is why
there are two implementations.

*Optional refinement, not initial scope:* a second-level Z-bin (64 coarse
entries, each the min/max over 64 fine bins, 256 B) would let the ray consumer
bound its far end by `hit_dist` for a couple of ALU. Worth it only if the
profiler shows the ray consumer candidate-bound after the tile mask tightens;
BeamPass may retire the consumer first.

---

## 3. Ordering, and the shadow-slot hazard

Z-binning requires light ids to be *view-depth-sorted* ids. This is the one part
of the migration with a hidden trap, and it must be designed for explicitly.

`Renderer::fixture_shadow_slots: [Option<usize>; MAX_FIXTURE_SHADOWS]` persists
**indices into `frame.fixture_cones`** across frames, and `assign_shadow_slots`
uses them for residency and eviction hysteresis. There is an undocumented
contract there already — *a fixture's identity is its position in the frame's
cone array* — and `FixtureCone` carries no id field to replace it with. If a
sort reorders that array, every fixture appears to change slots every frame,
every shadow map goes dirty every frame, and the shadow pass cost explodes with
no visible symptom other than frame time. That failure would be attributed to
the culler and it would not be the culler.

**Design: the sort order is private to `LightIndex`.** The module takes cones in
source order, sorts internally, and uploads *its own reordered copy* of the
light SoA (`LightCore` / `LightRest`) alongside the masks. Consumers'
`light_index` values are in sorted space and index the reordered SoA; nothing
outside the module ever sees a sorted id. `rests[].shadow_slot` rides along in
the reorder, so the shaders' shadow lookup is unaffected. Source order stays
canonical for `assign_shadow_slots`, `fixture_shadow_matrices`, caster culling,
and every existing test.

This means **LightIndex owns the light SoA upload**, which `gpu.rs` does today.
That is the correct boundary, not scope creep: the index and the light buffers
must agree on ordering, so they need one owner. It also pulls complexity
downward — the alternative is publishing a `sorted_to_source` remap and making
every consumer apply it, which is a pass-through variable in the Ousterhout
sense and would leak the sort into three shaders.

Flag while here: the "identity is array position" contract deserves a comment on
`Frame::fixture_cones` regardless of this work, and a `FixtureId` newtype is the
real fix. Out of scope, worth an issue.

---

## 4. API

### Rust

```rust
//! One clustered light index, in one structure, for every consumer that asks
//! "which fixtures can reach here".

/// Screen-tile edge, in full-resolution pixels. Consumers rendering at a
/// fraction of output resolution scale their fragment coordinate; the index is
/// never rebuilt per consumer resolution.
pub const TILE_SIZE: u32 = 8;
pub const MASK_WORDS: u32 = 16;   // 512 bits
pub const Z_BINS: u32 = 4096;

/// Everything the index is a function of. One struct so a caller cannot supply
/// the camera without the viewport it was framed for.
pub struct LightIndexInput<'a> {
    pub cones: &'a [FixtureCone],   // source order, <= MAX_FIXTURE_CONES
    pub camera: Camera,
    pub viewport: [u32; 2],         // full-resolution
    pub near: f32,
    pub far: f32,
}

pub struct LightIndex { /* persistent buffers, bind group, layout, pipelines */ }

impl LightIndex {
    pub fn new(device: &wgpu::Device) -> Self;

    /// Rebuild for this frame. Records the tile-assignment dispatch into
    /// `encoder`; the caller must submit it before any pass bound to
    /// [`Self::bind_group`].
    ///
    /// Infallible by construction: the viewport is clamped, the cone slice is
    /// truncated to the mask width, and the structure is fixed-size, so there
    /// is no allocation that can fail and no dimension that can overflow. This
    /// deliberately replaces `ClusterBuildError`.
    pub fn build(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input: LightIndexInput<'_>,
    ) -> &wgpu::BindGroup;

    /// Layout for pipeline creation, before any frame exists.
    pub fn layout(&self) -> &wgpu::BindGroupLayout;
}
```

Two things the signature says on purpose. There is no `Result` — the errors are
defined out of existence by the fixed-size structure, and every caller of
`get_or_build` today ends in `.expect(...)`. And there is no cache handle, no
key type, and no `rebuilt: bool` — the second return value that exists today
only so `gpu.rs` can decide whether to re-upload buffers the module now owns.

`LightIndexStats` is fetched separately (§7) so the hot path never touches it.

### WGSL

One prelude, `shaders/light_index.wgsl`, concatenated into `scene.wgsl`,
`haze.wgsl` and later the froxel injection shader — the same `include_str!`
composition already used for `fixture_light.wgsl` and `scene_bindings.wgsl`.

```wgsl
// Bindings: light_core, light_rest, tile_masks, z_bins, index_uniform.

struct LightCursor { word: u32, bits: u32, base: u32 }

/// Lights whose cone can reach this fragment at this view depth.
/// `frag_xy` is in full-resolution pixels; a half-res pass passes `frag.xy * 2`.
fn lights_at(frag_xy: vec2<f32>, view_depth: f32) -> LightCursor;

/// Lights whose cone can reach anywhere along this pixel's ray. Same words,
/// no depth restriction.
fn lights_along(frag_xy: vec2<f32>) -> LightCursor;

/// Returns false when exhausted. `id` indexes light_core / light_rest.
fn next_light(cursor: ptr<function, LightCursor>, id: ptr<function, u32>) -> bool;
```

Consumers become a `while (next_light(&c, &li))` loop over the existing body.
`haze.wgsl`'s current `tile.count` / `tile_light_indices[tile.offset + i]` walk
and `scene.wgsl`'s `cluster.offset .. cluster.offset + cluster.count` walk both
collapse into this, which is the point: the two shaders stop having their own
opinions about the index layout.

---

## 5. Bounds math — one implementation

Per light, on the CPU:

1. **Sanitise.** One `SanitizedCone`. Today `clusters::SanitizedCone` clamps
   range to `0.05..10_000` and `gpu::sanitize_fixture_cone` clamps it to
   `0.05..100`, and `haze_tiles` re-clamps to `0.05..100` a third time. Three
   clamps, two different ceilings, on the same field. One survives.
   (Julian's veto stands: this is sanitisation, not a beam-length clamp — the
   ceiling is a validity bound, and nothing scales it by content.)
2. **Tight cone bounding sphere**, §1.3(a):
   `a ≥ 45°: c = apex + r·cos a·dir, R = r·sin a`;
   `a < 45°: c = apex + (r/2cos a)·dir, R = r/(2cos a)`.
   Used *only* as the narrow-phase proxy, never as the broad-phase bound —
   §1.3(a)'s own correction establishes that its AABB is larger than the cone's.
3. **Broad phase: the cone's world AABB → clipped screen rect**, i.e. exactly
   today's `bounds_for` path with `box_corners` + `for_each_clipped_vertex`.
   This is proven, near-plane-correct, and tighter than the sphere.
4. **View-depth span** `[z0, z1]` → Z-bin span, and the sorted-order id range
   written into `z_bins`.

On the GPU, per (tile, light) pair: `cone_reaches_sphere` — Wronski, transcribed
from the existing Rust — against the **tile cell's bounding sphere, where the
cell is the tile's frustum wedge restricted to that light's own depth span**,
not to a fixed Z slice. That restriction is what makes the test well-conditioned
this time. Phase 1's failure was diagnosed precisely: at 16 log slices a cell at
20 m is ≈0.5 × 0.5 × 11.6 m, twenty times longer than wide, so its bounding
sphere is enormous relative to the cell and rejects almost nothing. A tile wedge
clipped to a 5 m cone's span at 8 px is not that shape.

**Design it twice, on the broad phase.** The alternative is Mara & McGuire's
closed-form screen bounds of a sphere — no clipping code at all, and it deletes
`box_corners` and `for_each_clipped_vertex` with their callers. It loses,
because the sphere's screen rect is roughly 2× the cone's for a 15° diagonal
cone, and the ray consumer has no narrow phase to recover it with. Keep the box
clip; port those ~60 lines to WGSL only if step 3 later moves to compute.

---

## 6. Cache and invalidation: there isn't one

**Rebuild every frame. Delete both caches.**

The evidence is not merely the soundness argument in §3.2. `topology_hash`
already invalidates 720/720 frames in the profiler's beams-at-camera case, and
in a live show fixtures move continuously — so the cache pays its key cost every
frame and returns nothing on exactly the frames that are over budget. The
quantised camera key bought ~16% fewer rebuilds while orbiting an *idle* rig and
nothing otherwise. Meanwhile the hash itself walks 512 cones × 9 floats twice
per frame, once per culler.

What the deletion subtracts, by name: `ClusterCache`, `ClusterCacheKey`,
`topology_hash`, `MOTION_QUANTUM_TILES`, `MOTION_REFERENCE_DEPTH`,
`BuildInput::motion_quantum`, `BuildInput::cache_key`, `HazeTileCache`,
`HazeTileKey`, `haze_tile_key`, `ClusterBuildError`, `ClusterStats::rebuilds`,
`Renderer::cluster_cache`, `Renderer::haze_tile_cache`, `Renderer::cluster_gpu`,
and the `rebuilt` bool threaded out of `get_or_build` into `gpu.rs`'s
buffer-reupload branch. Two cache-key concepts, two staleness contracts and two
FNV walks stop existing.

If "nothing in the scene changed, skip the frame" is ever wanted, it belongs at
the frame level where it can skip *everything*, not inside the culler where it
can only skip the culler and must be unsound to do it. Different layer,
different abstraction.

---

## 7. Build schedule — and why `atomicOr` is the wrong primitive here

§3.2 specifies "compute shader, `atomicOr`", following Drobot. With a hard
512-light ceiling we can do better, and the difference shows up on the case we
most care about.

- **Light-major** (one workgroup per light, threads stride the light's tile
  rect, `atomicOr` a bit): work is proportional to actual coverage, which is
  attractive — but the worst case is `beams-at-camera-128`, where every cone
  contains the eye and covers most of the screen. At 512 near-fullscreen cones
  that is up to 512 × 32,400 ≈ 16.6M atomics, all contending on 32,400 words,
  with catastrophic load imbalance across workgroups.
- **Tile-major** (one workgroup per 64 px big tile, its 64 threads each owning
  one 8 px sub-tile, looping the big tile's candidate lights and accumulating
  16 mask words **in registers**, then 16 plain stores): the inner loop has no
  atomics and no contention, the write pattern is perfectly coherent, and the
  worst case is 32,400 × 16 = 518k plain word stores. Load is balanced by
  construction.

**Pick tile-major.** The fixed ceiling is what licenses it: 512 lights is a
bounded loop, and a bounded loop with in-register accumulation beats an
unbounded one with atomics. Two dispatches:

1. **Big-tile prepass**, 64 px tiles (30 × 17 = 510 at 1080p): pure 2D
   rect-overlap against each light's screen rect, compacting a candidate list
   per big tile. HDRP's fine-pruning prepass; ~16× candidate reduction in the
   typical case, zero correctness risk. §3.2 lists this as optional escalation —
   promote it to initial scope. It is what makes the tile-major inner loop
   affordable, and it is thirty lines.
2. **Tile fill**, as above, with the Wronski narrow test.

Honest statement of the limit: when every cone genuinely covers the screen, no
culler helps, and neither the prepass nor the narrow test rejects anything. What
changes for that frame is that the cost becomes 518k coherent GPU stores instead
of ~3.7M CSR writes on one CPU core, which is where the measured 4–5 ms lives.

Cost estimate, arithmetic exposed rather than cited: `transport-512` currently
produces 5.79M references, so the narrow test runs at most ~16.6M times and the
mask write ~518k times; at M3 Max's ALU throughput that is a small fraction of a
millisecond, and the §3.2 estimate of 0.2–0.4 ms is consistent. `cpu_cluster`
should go to approximately zero — the CPU half is one 512-element sort, a 16 KB
Z-bin fill, and 512 screen-rect computations.

**Determinism survives, and improves.** A mask is order-independent by
construction, so the parallel build has no ordering discipline to get right —
unlike the CSR list, whose reproducibility depends on visiting lights in source
order. The profiler's `samples_fnv64` and the contract goldens stay meaningful.

---

## 8. Metrics

`ClusterStats`'s fields do not survive the structure change and must not be
renamed into it. `occupied_clusters` and `light_references` have no meaning for
a fixed-width mask. Replacements:

| new field | meaning |
|---|---|
| `mean_lights_per_tile` | popcount of the 2D mask over non-empty tiles — the ray consumer's cost, and the successor to today's headline number |
| `max_lights_per_tile` | worst tile; the tail |
| `mean_lights_per_fragment` | popcount of `mask ∩ zbin range`, accumulated by `scene.wgsl` under the existing debug flag — **the number that actually predicts shading cost**, and the only honest successor to `mean_lights_per_cluster` |

All three are GPU-side counters in a small buffer, read back **one frame late**
via async map, and only under the profiler's flag. Never on the live path.

The profile artifact's `budgets.mean_lights_per_cluster` field is renamed, the
schema version bumped, and the golden re-baselined per §"Re-baselining the
profile golden". The committed
`goldens/volumetric-profile-m3-max.json` is in any case stale — it records
`wgpu_lock: 26.0.1`, `cpu_cluster.p95 = 0.0` on every case, and lacks the
`zoom-*`, `beams-at-camera-128` and `geometry-*` cases entirely. **Re-baseline
it before starting**, or the before/after numbers in §10 have no before.

---

## 9. Migration, at file granularity

Each step compiles and passes goldens on its own.

**Step 1 — the module, CPU only, no consumers.**
- Add `gpui/crates/render/src/light_index.rs`: `TILE_SIZE`, `MASK_WORDS`,
  `Z_BINS`, the const-assert against `MAX_FIXTURE_CONES`, `LightIndexInput`,
  the sanitiser, the sphere, the broad-phase rect (moved from
  `clusters::BuildInput::bounds_for`), the Z-sort, the Z-bin fill, and a
  **reference CPU tile-mask builder**.
- Move `box_corners`, `for_each_clipped_vertex` and `cone_reaches_sphere` into
  it unchanged; they keep `pub(crate)` and `gpu.rs:2685`'s caster cull keeps
  calling `cone_reaches_sphere` directly.
- Unit tests: conservativeness (sampled cone-interior points are present in
  their tile's mask and their fragment's `mask ∩ zbin`), determinism, the
  near-straddle case, the eye-inside case.
- `lib.rs`: add `pub mod light_index;`. `clusters` stays, untouched.

**Step 2 — the GPU builder, validated against step 1.**
- Add `shaders/light_index_build.wgsl` (big-tile prepass + tile fill) and the
  pipelines in `light_index.rs`.
- Add a test that runs both builders on the same input and asserts the mask
  buffers are **bit-identical**. This is why step 1's reference builder exists,
  and it is worth keeping permanently — it is cheap and it is the only thing
  that will catch a WGSL/Rust drift in the Wronski test.

**Step 3 — cut `haze.wgsl` over.** The lower-risk consumer: it has no depth
component, so it exercises the mask alone.
- Add `shaders/light_index.wgsl`; concatenate into `haze.wgsl` alongside
  `fixture_light.wgsl`.
- Replace the `tile_headers` / `tile_light_indices` walk with
  `lights_along(frag.xy * inv_haze_scale)`.
- `gpu.rs`: delete `haze_tiles`, `haze_tile_key`, `HazeTileKey`,
  `HazeTileCache`, `HAZE_TILE_SIZE`, `Renderer::haze_tile_cache`, the
  `HazeUniform::tiles` field and its three tests (`gpu.rs:3738–3860`).
- Gate: contract goldens **bit-identical** (§10).

**Step 4 — cut `scene.wgsl` over.**
- Concatenate the same prelude; replace `surface_cluster_index` and the
  `cluster.offset..` walk with `lights_at(in.clip.xy, view_depth)`.
- Rework the `cluster_debug` occupancy view to popcount the mask.
- `gpu.rs`: `LightIndex` takes over the `LightCore`/`LightRest` upload
  (`gpu.rs:2062–2094`) and publishes one bind group replacing `cluster_bg`,
  `SurfaceClusterUniform`, `cluster_gpu`, and the `light-core`/`light-rest`
  `storage()` calls.
- Gate: contract goldens bit-identical; `mean_lights_per_fragment` gate (§10).

**Step 5 — delete.**
- Remove `gpui/crates/render/src/clusters.rs` and `pub mod clusters;`.
- Remove `ClusterStats` from `lib.rs`'s re-exports; add `LightIndexStats`.
- `bin/profile-volumetrics.rs`: rename the budget field, re-point the stats,
  bump the artifact schema.
- Sweep `CLUSTER_TILE_SIZE` / `CLUSTER_DEPTH_SLICES` out of `gpu.rs`'s tests
  (`gpu.rs:4115`).

**Step 6 — enable the narrow phase, measure, keep or revert.**
The Wronski per-tile test lands behind a const in step 2 and is switched on
here as its own measurement, so that if it again fails to pay we learn that
about *this* structure and not about the pair of changes together. Phase 1's
lesson, applied.

---

## 10. Gating measurements

Run `profile-volumetrics` in `--release`, re-baselined first (§8).

**Correctness gate, and it is the sharp one.** Both cullers are conservative and
so is the new index; `scene.wgsl` hard-continues on `angular <= 0.0` and
`haze.wgsl` hard-continues on a missed sphere, so a light that is *included but
does not reach* contributes exactly zero. Therefore:

> **All contract goldens must be bit-identical across steps 3 and 4.** Any
> pixel that moves means the index dropped a light it should have kept.

That is a stronger and cheaper gate than any tolerance-based comparison, it is
available at every step, and it is the reason to sequence the consumers
separately rather than cut both over at once.

*Step 3 measured outcome:* one caveat the gate as written missed — the mask
walk visits lights in depth-sorted order while the old tile list was
source-ordered, and floating-point accumulation is order-sensitive. Measured
across every contract golden at the haze cutover: one image
(`volumetric-performance-smooth`, 120 cones) moved **8 pixels by 1 LSB**; all
others byte-identical. That signature is summation reorder, not a dropped
light (a dropped light is a dark region, not isolated ±1s); goldens were
re-baselined once with this note. The gate stays byte-exact from here.

*Step 4 measured outcome:* the SoA reorder moves both consumers' accumulation
to depth-sorted order, and the scene target is fp16 — reordering ~100 adds in
half precision shifts overlapping-light pixels by 1–3 8-bit steps (worst 27 on
0.04% of channels, direction balanced). Two goldens moved
(`overlapping-beams`, `volumetric-performance-smooth`). Proof this is the
reorder and not the culler: rendering with the index bypassed (every light
walked per fragment) is **bit-identical** to the culled render on both images
— the index drops nothing. Re-baselined once more with this note.

*Step 6 measured outcome:* the narrow phase (Wronski cone vs tile-wedge
sphere clipped to the light's own depth span) is **on**, in both builders,
under the same injected `NARROW_PHASE` constant. The bit-identity test covers
it and passes; every contract golden is byte-identical with it enabled, and
`cpu_cluster` did not move (the CPU half only runs in the reference builder).
The whole-suite outcome of steps 1–6 on the profiler: `cpu_cluster` p95
7.93 → 0.17 ms and cold build 7.03 → 0.14 ms on `transport-512`,
`cpu_encode_submit` p95 10.63 → 0.45 ms, `gpu_total` p95 32.28 → 0.39 ms
(the CSR upload stall is gone), per-frame index allocation 23.2 MB → 0.
The §8 GPU counter pair is in (`light_index_fragment_counters`, surface pass,
profiler flag only) and the §10 falsification gate has its number:
`transport-128` measures **`mean_lights_per_fragment` = 48.8** against the
CSR era's 104 — a 2.1×, short of the pre-registered 3.5×. Per §10's own terms
the §3.2 premise is *partially* falsified: 8 px tiles + 4096 uniform Z-bins do
not make the per-fragment set small on this scene, because wide up-throwing
beams span most of the view-depth range, so the sorted-id Z-bin ranges stay
wide (208.9 at 512 cones, 76.9 eye-inside). The economics no longer bite —
the pass runs 0.34 ms p95 — but BeamPass's §2.5 pre-registration should use
48.8, not a hoped-for <30, as `B̄` when it evaluates the proxy win.

**Performance gates:**

| case | metric | today | gate |
|---|---|---|---|
| `beams-at-camera-128` | `cpu_cluster` p95 | 4–5 ms | **< 0.3 ms** |
| `beams-at-camera-128` | `gpu_total` max | ~48 ms | must not regress; expect −4 ms from CPU relief only |
| `transport-512` | cold build | 13.8 ms | **< 1 ms** |
| `transport-512` | `gpu_volumetric` p95 | 5.07 ms | **≤ 4.0 ms** |
| `transport-128` | `mean_lights_per_fragment` | 104 (as `mean_lights_per_cluster`) | **< 30**, i.e. ≥ 3.5× |
| all | index build GPU pass | — | **< 0.5 ms** p95, new budgeted field |
| all | per-frame index allocation | 6.8–23.2 MB | **0 bytes** |

The `mean_lights_per_fragment` gate is the falsification test for §3.2's whole
premise. Phase 1's equivalent gate failed at 1.8× on the CSR structure, and the
diagnosis was that the cells were Z-splinters rather than that the bound was
loose. If 8 px tiles and 4096 Z-bins do not clear 3.5×, that diagnosis was wrong
and the doc needs revising before BeamPass builds on it. Record the number
either way.

`beams-at-camera-128` is expected to improve on CPU and be roughly flat on GPU —
it is a fill-bound case, and fill is BeamPass's problem, not this one. Stating
that in advance so a flat GPU number is not read as a failure. `transport-512`
is where the GPU shading win should show, via tighter fragment light sets.

---

## 11. Risks

- **Shadow-slot residency (§3).** The one silent failure mode. Mitigated by
  keeping the sort private; verify with a test that asserts
  `assign_shadow_slots` returns a stable assignment across frames where only
  view depth ordering changed.
- **Register pressure in the tile-fill inner loop.** 16 live mask words per
  thread plus the Wronski test's operands. If occupancy suffers, split the fill
  into two dispatches of 8 words each — the candidate list is already compacted,
  so the second pass re-reads it cheaply.
- **Half-res coordinate scaling.** `haze_resolution` is a float in 0.25..1.0, so
  `frag.xy * inv_scale` must round consistently with the full-res tiling or a
  half-res pixel can read a neighbouring tile at the edge. Conservative rounding
  (take the tile of the *pixel footprint's* min and max, OR both masks) costs
  one extra mask read and removes the class. Prefer that to getting the rounding
  exactly right, and delete it with the consumer when BeamPass lands.
- **Golden bit-identity may not hold if any consumer path is not hard-zeroed.**
  Verified for `scene.wgsl:381` and `haze.wgsl`'s sphere reject; re-verify for
  the ambient bed and the `cluster_debug` view before relying on the gate.

## 12. Open questions

- Does the froxel injection pass (§3.4) want the same 8 px tiles, or its own
  coarser grid? A 160×90×64 froxel grid at 12 px does not align to 8 px tiles.
  Deferred — it is a consumer question, and the mask is readable at any
  granularity by ORing the tiles a froxel covers.
- Subgroup scalarisation (`subgroupOr`, `subgroupBroadcastFirst`) on the
  consumer side. §3.2 flags it; it is orthogonal to this design and should be
  measured after, against the non-scalarised baseline.
