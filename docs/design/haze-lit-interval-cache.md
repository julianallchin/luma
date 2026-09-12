# Native haze: lit-interval cache

Status: design, 2026-09-10. Not built. Written after the Gasworks 75 FPS
campaign (harness/perf/mac-gasworks-2026-09-09/README.md, "Where the frame
is") exhausted every per-loop optimisation of the native haze pass.

## What is cached

For one pixel and one shadowed light, `beam_shadow_integral`
(`shaders/beam_transport.wgsl`) walks the camera ray through the light's
shadow-map hierarchy and produces a list of lit intervals in ray distance
`t`. It then calls `lit_interval` on each. The walk is ~5.1 ms of the
11.5 ms pass (omission ladder, run-20260910-211344-traversal). The
quadrature is the other ~6 ms.

The walk depends on: camera position and ray direction, the pixel's opaque
depth (through `beam_span`), the light's position, range and field angle,
and the light's shadow map. It does not depend on haze, wind, time,
intensity or colour.

The cache stores the walk's output: the lit `t` intervals. The quadrature
is recomputed every frame with fresh haze. This is exact by construction:
the cached path calls `lit_interval` on the same intervals in the same
order as the traversal would.

It is not a glow cache. The glow cache (whole integral, scaled by
intensity) is a separate, larger, non-exact step and is out of scope here.

## Why per-frame invalidation is enough

Every input to the walk is either global to the frame or per light:

| Input | Granularity | Already tracked? |
|---|---|---|
| camera eye + view-projection | frame | `Globals`; bits comparable |
| opaque depth buffer | frame | reuse survey's `opaque_depth_inputs_identical` uses draw/mesh identity; the renderer has the same inputs |
| light position, direction, field | per light | `ShadowCacheKey.matrix_bits` (shadow matrix is built from them) |
| light range, `cos_field` | per light | not in the shadow key; add their bits |
| shadow map contents | per light | `ShadowCacheKey.caster_hash`; `fixture_shadow_dirty` |

So validity is one bit per shadow slot per frame, computed on the CPU:

```
slot_valid[s] = camera_bits == prev_camera_bits
             && depth_inputs_hash == prev_depth_inputs_hash
             && slot_key[s] == prev_slot_key[s]       // shadow key + range + cos_field bits
             && slot_resident_last_frame[s]
```

No per-pixel keys, no per-pixel compares. A camera move clears every bit.
A moving head clears its own bit. A dimmer or colour change clears nothing.

Frame-to-frame in the 3 s suite: camera identical 23/23, depth inputs
identical 23/23, 146 of 146–154 cones geometry-identical, 0–4 shadow maps
redrawn. Expected hit rate on playback with a still camera: ~95% of pairs.

## Why shadow slot is the key

`haze_at` iterates lights by light-index id `li`, which is rebuilt every
frame. In the survey only 20 of 146 geometry-identical cones kept their
index. `li` cannot address the cache.

`assign_shadow_slots` (`shadow.rs:69`) keeps residents in their slot unless
a challenger beats them by a 1.25× margin, and `assign_cached_slots` keeps
them when shadows are cached. Only lights with a slot do the walk. So the
slot number is the stable id, and `light_rest[li].shadow_slot` already maps
`li` to it in the shader.

Slot eviction is a key change, which clears the bit. Correct, no extra
logic.

## Layout

The hard part is memory traffic, not logic. Every cached pair is read
every frame, so bytes per pair × pairs per frame is a per-frame bandwidth
cost that comes straight off the 5.1 ms ceiling.

Numbers from the counters on gasworks-get-lucky-3s-close:

| | per frame |
|---|---|
| shadowed (pixel, light) pairs | 20.17 M |
| whole-lit at first lookup | 12.80 M (63%) |
| non-empty `lit_interval` calls | 21.26 M (≈1.05 per pair, tails included) |

Two-tier layout, allocated per (shadow slot, 8×8 screen tile):

1. **Tile block header, always present.** Per pixel in the tile: 2 bits
   (`whole_lit`, `has_payload`). 64 pixels → 16 bytes. Plus a `u32`
   payload offset. 20 bytes per block.
2. **Payload, only for pixels not whole-lit.** Fixed `K` intervals of two
   `f32` each, plus a `u8` count. `K` is decided by the histogram probe
   below; expected 2. With `K = 2`: 17 bytes, pad to 20.

Blocks needed = candidate tile visits / 64 ≈ 29.8 M / 64 ≈ 466 K blocks
≈ 9 MB of headers. Payload ≈ 0.37 × 20.17 M × 20 B ≈ 150 MB. Read traffic
per frame ≈ 160 MB ≈ 0.5–0.8 ms on this GPU. Net expected win: 5.1 − 0.7
− miss overhead ≈ **3–4 ms on a still camera**. That takes the 3 s replay
from ~26.3 to ~22.5 ms. It does not reach 13.3 ms.

Intervals stay `f32`. Quantising them to `u16` fractions of the span would
halve payload but moves quadrature endpoints by ~1.5 mm and forfeits
bit-exactness. Not worth it for the first build.

A pair whose interval count exceeds `K` stores `has_payload = 0`,
`whole_lit = 0` and takes the traversal every frame. The probe says how
many.

Block allocation: the light index already knows each light's tile set
(`light_index_build.wgsl` writes the per-tile mask). Give each resident
slot a contiguous block range sized to its tile count, rebuilt on the CPU
only when the slot's tile set changes (which is a key change anyway). A
flat `(slot, tile) → block index` table of 256 × tiles `u32` ≈ 256 ×
48.5 K × 4 B ≈ 50 MB is too big; instead store per slot a tile-rect origin
and stride, so `block = base[slot] + (ty − rect.y) × rect.w + (tx −
rect.x)`. The rect is the cone's projected screen AABB, already computed
for the index.

## Miss path and write cost

On a miss the shader traverses as today and writes the entry. The write
is one extra store per pair on the miss frame only. When every slot
misses (camera moved), the frame pays today's cost plus the writes,
estimated +0.3–0.5 ms. So orbiting is not faster than today and is
slightly slower. This is the trade the user accepted in discussion; it
must be measured on a camera-pan suite, not assumed.

If the write overhead measures worse than ~0.5 ms, gate writes on a
"camera settled for one frame" flag so a continuous orbit never writes.

## Shader change

One function, `beam_scatter`, on the `NATIVE_DETERMINISTIC` path:

```
if haze.shadow.x <= 0.0 { return lit_interval(full) }
let slot = shadow slot of li
if slot_valid[slot] {
    read header bit for this pixel
    whole_lit    -> return lit_interval(full)
    has_payload  -> sum lit_interval over stored intervals (same order as traversal)
    else         -> fall through to traversal, do not write
} else {
    traverse (unchanged code), collecting intervals into a private array of K
    if count <= K { write entry } else { write has_payload = 0 }
    return sum
}
```

The traversal already accumulates `lit_start`/`lit_end` and calls
`lit_interval` per merged run plus two tail calls. The cached path must
replay exactly those calls in that order: tails first, then runs. The
tails `[full.x, span.x]` and `[span.y, full.y]` come from the frustum
clip, which is camera and light dependent but cheap; recompute them
rather than store them, or store them as intervals 0 and 1. Storing is
simpler and guarantees order. Then `K` counts tails plus runs, so expect
`K = 4` to be needed; the probe decides.

Warning from today: adding a `bool` parameter to `beam_scatter` changed
~1800 px/frame under Metal fast-math. Exactness is verified by pixels,
never assumed. Keep the traversal code byte-identical and add the cache
branch around it.

## Probe before building

One instrumented capture on the 3 s close view, using the existing
`LUMA_HAZE_WORK_COUNTS` machinery with two counters repurposed:

1. Histogram of `lit_interval` calls per (pixel, light) pair, including
   tails. Sets `K` and the overflow fraction.
2. Pairs per pixel, max and p99. Confirms the tile-block sizing.

If the overflow fraction at `K = 4` is above ~5%, the layout needs a
variable-length pool and the design should be revisited before code.

## Gate

Same harness, same rules:

- Control = current tree binary saved before edits.
- 3 s and 4.9 s replays, control vs candidate, `audit_replay.py` without
  `--measure-differences`: 0 changed pixels required. Reverse-order pair.
- Camera-pan case from `native-suite.json` to measure the miss path.
- Counters: traversal counters (2, 3, 6) must fall on hit frames; taps (5)
  must be identical.
- Accept if exact and the 3 s GPU median drops ≥ 2.0 ms in both orders.

## What this does not do

- It does not touch scene shading (~13 ms of real work, on the critical
  path in parallel with the fog chain).
- It does not reduce the quadrature (~6 ms).
- It does nothing while the camera moves.

After this step the frame is ~22 ms. Reaching 13.3 ms still needs both the
scene fixture loop and the quadrature roughly halved, and neither has a
design yet.
