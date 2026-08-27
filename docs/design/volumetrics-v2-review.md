# Volumetrics v2 — adversarial review (2026-08-25)

Gap analysis of `volumetrics-v2.md` against current wgpu, Apple GPU documentation,
and the previz/games corpus. Labels: **[V]** verified from source/spec/manual,
**[I]** inferred. Ranked table at the end.

**Headline: the doc's biggest error is not a missing technique, it's a stale constraint table.** §8.1 and §2.5 are the load-bearing "what wgpu can't do" tables that justify most of the shadow architecture, and three of their rows are false as of wgpu 28/30. wgpu 29.0.4 is *already in your lockfile*. Second-biggest: §3.1's BeamPass has an unbounded worst case that is exactly the all-lights-on case, and §14.2 contains the evidence without noticing it. Third: the category's actual answer to all-lights-on is a closed-loop frame-time governor, which the doc mentions in one clause and never adopts.

---

## Tier 1 — architectural gaps

### 1. §8.1's constraint table is four majors stale, and it invalidates the premise of the shadow plan

**[V]**, read from wgpu source, not docs:

| §8.1 says | actually |
|---|---|
| `Features::MULTIVIEW` ❌ Vulkan/GL only | **Landed v28.0.0 (2025-12-17, PR #8206).** `wgpu-hal/src/metal/adapter.rs:1325` sets `F::MULTIVIEW` when `supported_vertex_amplification_factor > 1` |
| `CLIP_DISTANCES` ❌ Vulkan/GL only | **Landed v30.0.0 (2026-07-01, PR #9270).** `adapter.rs:1221` sets it **unconditionally** on Metal |
| mesh shaders ❌ don't exist in wgpu | **Landed v30.0.0 (PR #8739)**, WGSL included |

The number that matters: **[V]** Apple Metal Feature Set Tables (May 2026 rev, p.8) — "Maximum vertex count for vertex amplification" = **8 on Apple7–Apple10** (Apple7 = M1). Every Apple silicon Mac gives 8 views per pass. wgpu 28 also added `SELECTIVE_MULTIVIEW` + `multiview_mask`. Latest release v30.0.1 (2026-08-21).

Why this matters: §7 (apex anchoring), §8 (merged index buffer), §8.2 (one atlas one pass), §8.3 (compute raster) are workarounds for "we cannot batch views." An 8× cut in pass and draw count needs no algorithm. **[V]** hardware clip distances measured 4–5× faster than fragment-discard for atlas tile clipping (Unity emulate-SetViewport thread).

Caveats **[V]**: mesh shaders do not restore layer routing (no `render_target_array_index` in WGSL); texture 64-bit atomics tightened to Apple9-only on trunk while buffer `SHADER_INT64_ATOMIC_MIN_MAX` is Apple8+/Mac2.

Impact on all-lights-on: shadow passes 16→2, encode cost roughly /8. Highest-leverage item in the analysis; costs a version bump plus 26→30 API churn.

### 2. §3.1 BeamPass: the doc refutes itself and doesn't notice

§3.1: "The culling problem for beams disappears. The rasterizer **is** the cull, and it is exact." §14.2, twenty pages later, quotes Capture: fixtures focused into the camera "effectively affect the entire screen," and Depence ranking lights shining into the camera as its #2 cost — cited as evidence they use proxies, but equally evidence that **proxy raster's worst case is unbounded**, and the all-lights-on strobe cue is where it fires.

Three things the doc doesn't have:

- **[V]** Apple WWDC20 10602 on HSR + blending: "the HSR block will need to flush the pixels covered by the translucent primitive… The GPU cannot defer fragment shading any longer." **TBDR gives additive-blended geometry exactly zero overdraw reduction.** Apple Silicon is the worst platform for this technique. Nothing in §2.5 says this.
- **[V]** Olsson & Assarsson, *Tiled Shading*, JGT 2011: at 924 lights, TiledDeferred 15.7 ms vs Deferred+Stencil 18.4 ms — only ~17% — and "the stencil optimization is the most efficient at culling work." Proxy raster does strictly LESS shading work than tiled; tiled wins on G-buffer re-read bandwidth, which luma does not pay (ALU integrand, no per-light memory traffic). §3.1's average win is smaller than claimed AND its tail is worse than what it replaces.
- **[I, airtight]** Additive has no early-out — no saturation term to test, no fixed-function mid-blend read of the HDR target. Front-to-back sorting buys nothing.

§3.1 should carry an explicit "when NOT to use this" and a per-tile fallback; the profiler needs the strobe case before the rewrite lands.

Precedent for the hybrid: **[V]** DOOM Eternal (SIGGRAPH 2020) — each fragment selects between cluster and tile lists, "never worse than clustered." Closest published thing to BeamPass, uncited: **[V]** Insomniac **Light Linked List** (Bezrati, SIGGRAPH 2014 / GPU Pro 6): rasterize low-tess light shells, software depth test, per-pixel list at **1/8 screen res**, 8 bytes/fragment, 256 lights/frame, avg 40 lights/pixel, <0.16 ms at 1080p, 7.25 MB — the middle term between §3.1 and §3.2.

### 3. No frame-time governor — and that IS the category's answer to all-lights-on

**[V]** Capture Adaptive quality: "resolution and quality… automatically adapted to the current lighting conditions and the performance of the computer," plus a Prioritisation slider and a live perf readout. **[V]** WYSIWYG Performance tab is the cleanest spec: Auto-Adjust + Target FPS + Min. Quality — setpoint, controlled variable, floor. **[V]** UE dynamic resolution ships the damping terms worth copying verbatim: `FrameTimeBudget` 33.3 ms, `TargetedGPUHeadRoomPercentage` 10, `MinScreenPercentage` 50, `MinResolutionChangePeriod` 8 frames, `MaxConsecutiveOverBudgetGPUFrameCount` 2. **[V]** Niagara is the best "drop effects, not resolution" governor; its documented trap is spawn-order FCFS culling — want rank-then-clamp by screen coverage × intensity.

Warnings: **[V]** WYSIWYG's premium beam path opts *out* of the governor; **[V]** everything else adaptive in previz is a stateless function of a scene parameter (Epic: samples from zoom; Notch: shadow res from distance; grandMA3: LOD from vertex count) — deterministic and cross-fade-safe. That's the conservative pattern; the closed loop is the aggressive one.

Doc coverage: one subordinate clause in §14.3, never a design item. For "runs on weak laptops," the highest value-per-line-of-code item in this report.

### 4. §7.3 dismisses the strongest form of apex anchoring

A 6×128² cube is 98k texels vs one 256² perspective map at 65k — 1.5× the memory, and it covers **every rotation**, so for static venue geometry it renders **once, at rig load**. **[I]** Break-even vs a per-frame 256² map is ~1–2 Hz refresh. §13's ✗ on DPSM is correct (per-vertex paraboloid warp bends straight edges), but cube and **octahedral** are not DPSM; octahedral is a single square 2D texture, atlas-friendly, one manageable seam. Neither appears in the doc. **[V]** Nobody in games caches this (HDRP: "when the light direction changes, all of the caches will be invalidated") because nobody has 100 lights rotating about fixed apexes over static geometry — §7 found a novel property; §7.3 under-exploits it.

Impact: "500 dynamic shadow maps/frame" → "500 static maps built once + a thin dynamic layer for performers." Larger than every batching win in the doc combined.

---

## Tier 2 — levers the doc names but never adopts

### 5. Beam-length clamp — the category's #1 lever, not in the plan
§14.3 names it; §17 doesn't adopt it. `gpu.rs:2019` clamps range to 0.05..100 and nothing scales it. Depence: global Max Spotlight Range; UE DMX: `Light Distance Max`. Attacks fill twice: smaller proxy AND fewer samples.

### 6. Intensity culling — right instinct, weakest implementation
`gpu.rs:1992` is a hard-zero test. **[V]** L8 ships `LUX Minimum`, a luminance-threshold cull. The valuable form: **shrink `range` by intensity** (`range_eff = range · sqrt(intensity/ε)`) so the bounding volume itself contracts — tightens the tile cull today and the proxy area under §3.1. A rig at 30% dimmer gets ~45% range cut.

### 7. f16 — available today, absent from every shader
**[V]** `SHADER_F16` is in wgpu-hal 26's unconditional Metal base set; naga 26 lowers to MSL `half`. Zero f16 in haze.wgsl/scene.wgsl. Calibration: **[I]** ~18% ALU (Turner's metal-benchmarks), not 2×; M3+ Dynamic Caching weakens the occupancy argument. Budget 10–20% on the fragment stage.

### 8. `StoreOp::Discard` on `msaa_depth` — one line
**[V]** `gpu.rs:2771` stores msaa_depth (via `depth_attachment()`, StoreOp::Store at 5004) and it is **never bound as a texture**. ~33 MB of tile→DRAM writeback per frame at 1080p 4×MSAA depth32. wgpu maps Discard → `MTLStoreAction::DontCare`. §3.5's budget discipline covers buffer churn and says nothing about attachment store actions — on TBDR the larger term.

---

## Tier 3 — techniques §2/§14 missed

### 9. Multi-emitter merge LOD — unreconciled contradiction
§14.3 flags grandMA3's Single Beam Dynamic Gobo as "should consider"; §17 drops it; §15.2/§16 argue per-cell articulation, which multiplies apertures. **[V]** Capture: "it's the number of apertures, not the number of fixtures, that counts." Reconciliation: model per-cell (§16 right), render merged when cells agree (§14.3 right).

### 10. Emissive-only third bucket
Five products, five names: Capture `Throws light` off · Vision Force Emissive · grandMA3 `Wash`→`Glow` · Notch `Simple Scattering` · BlenderDMX "Display beams" off. §3.0's partition invariant wants a third bucket: visible glow, zero volumetric fill, zero surface contribution — the LOD rung the vendors reach for when a beam points at the camera. Also grandMA3's `Line` rung: zero-fill beam that still communicates aim.

### 11. Receiver-mask caster culling — two uncited papers
**[V]** Olsson et al., "More Efficient Virtual Shadow Maps for Many Lights," TVCG 2015: 356 lights, 2.58M tris, 34 ms; per-face 32×32 receiver bitmasks ("always at least six times more efficient"); batch sweet spot 32–512 tris. **[V]** Bittner et al., I3D 2011: 3–10×. Phase 3 has only the emitter-side cone test; the receiver-side test is where the ≥6× lives. Unread: Treyarch GDC 2021 "Shadows of Cold War" (fixed budget, unrestricted light count).

### 12. wgpu landmine
**[V]** gfx-rs/wgpu#8768: ~96 bytes leaked per render-pass creation on Metal (M1–M3, Dawn too). Argues for one-atlas-one-pass independently of perf. Encoder cost ceiling ~110 µs (Apple forums 133454): 16 passes ≈ 1.8 ms — passes are not the bottleneck, the 33k draws are.

---

## Tier 4 — interim levers, and dead ends

- Intensity-aware tile culling → keep, but shrink range (finding 6), don't just skip zeros.
- 1 subframe under load → make it a governor output (finding 3), not a manual cliff.
- Fewer default steps → wrong shape; every shipped product scales steps by a scene parameter (UE: zoom angle). §3.1's solid-angle-adaptive counts supersede this lever.
- Unnamed, in order: beam-length clamp; StoreOp::Discard; **per-pixel beam-count histogram in the profiler** (Xcode exposes fragment-invocations ÷ pixels-stored; §3.1's whole cost model is Σ beam screen area and it is unmeasured).
- Sun et al. F(u,v) LUT: re-rank from curiosity to **worst-case lever** — strobe/blinder cues are open white, un-goboed, exactly the LUT's fast path.
- **Dead ends [V]:** VRS (absent on Apple/wgpu; rasterization-rate maps effectively visionOS-only); memoryless targets (absent in 26, landed v28 as TRANSIENT — footprint not speed; Discard already saves the bandwidth); programmable blending/tile shaders (unreachable; gpuweb #396/#442 parked since 2019); software-raster shadows (venue tris ~400 px ≫ every published <32–64 px crossover; if ever, 32-bit atomicMin on R32Uint suffices — no 64-bit texture atomics needed); DPSM (correctly ruled out); additive early-out (no mechanism).

## Field data

**[V]** MA forum 6031 (gMA3 onPC 1.6.1.3): x4 Bar alone 63 fps; JDC-1 alone 63 fps; both together **3 fps**, GPU *below* idle — pipeline state-switch serialization, not saturation. Design rule: **one beam pipeline, not one per beam type.** Same rig: 11 fps front view vs 37 fps top view — finding 2's unbounded worst case measured in the wild.

## Ranked

| # | finding | doc status | expected impact on all-lights-on |
|---|---|---|---|
| 1 | wgpu upgrade: 8-way MULTIVIEW, Metal CLIP_DISTANCES, mesh shaders | §8.1/§2.5 wrong | 8× shadow pass+draw count |
| 2 | Rotation-invariant cube/octahedral apex maps | §7.3 dismisses without arithmetic | static venue shadows built once; retires throttle |
| 3 | Closed-loop frame-time governor | one clause, never a design item | makes "weak laptops" a spec |
| 4 | BeamPass unbounded worst case; per-tile fallback + "when not to" | §3.1 claims opposite; §14.2 holds refutation | prevents regression on target case |
| 5 | Emissive-only bucket + Line rung | absent; five products ship it | zero-fill LOD for beam-at-camera |
| 6 | Beam-length clamp | named, never adopted | large, cheap, attacks fill twice |
| 7 | Range-shrinking intensity cull (L8 LUX Minimum) | hard-zero only | ~45% range cut at 30% dimmer |
| 8 | Receiver-mask caster culling (TVCG 2015, I3D 2011) | uncited | ≥6× shadow triangles |
| 9 | Per-pixel beam-count histogram in profiler | absent | prerequisite for 4 and 6 |
| 10 | StoreOp::Discard on msaa_depth | absent | ~33 MB/frame, one line |
| 11 | f16 in beam integrand | absent from §2.5 | 10–20% fragment stage |
| 12 | Multi-emitter merge LOD, reconciled with §16 | flagged, dropped, contradicted | apertures, not fixtures |
| 13 | Light Linked List as §3.1/§3.2 middle term | uncited | <0.16 ms at 1080p |
| 14 | Sun et al. LUT as worst-case lever | filed as marginal | applies on the strobe frame |
