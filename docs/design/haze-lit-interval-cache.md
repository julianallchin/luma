# Native haze: lit-interval cache

Status: built, and the only live haze path. `gpui/crates/render/src/interval_cache.rs`
holds the CPU side, `gpu/haze_cache.rs` the per-frame planning and the pools,
`shaders/haze_cache.wgsl` the GPU side, and `haze_compact.rs` /
`shaders/haze_compact.wgsl` the residual compaction built on it. Every live
frame and capture (gobos and multi-subframe exports included) runs the compute
kernels with this cache, compaction, 2×2 quad sharing and 8-pixel far-field
cells (16 at medium quality; low quality marches nothing); the per-ray fragment
pass is only `Renderer::render_reference`. There is no switch to turn the cache
off.

The device must offer subgroup operations. An 8×4 block that is one
subgroup ballots and shuffles through it and may share quads; a block split
over narrower subgroups (lavapipe's 8 lanes, Intel's 8 or 16) exchanges
ballots through workgroup memory and integrates every pixel exactly. The
sharing decision is taken from the recorded traversal on write and uncached
frames and from the header on read frames, so the image does not depend on
cache history.

## What is cached

For one pixel and one shadowed light, `beam_shadow_integral`
(`shaders/beam_transport.wgsl`) walks the camera ray through the light's
shadow-map hierarchy and produces a list of lit intervals in ray distance
`t`. It then calls `lit_interval` on each.

The walk depends on: camera position and ray direction, the pixel's opaque
depth (through `beam_span`), the light's position, range and field angle,
and the light's shadow map. It does not depend on haze, wind, time,
intensity or colour.

The cache stores the walk's output: the lit `t` intervals. The quadrature
is recomputed every frame with fresh haze. This is exact by construction:
the cached path calls `lit_interval` on the same intervals in the same
order as the traversal would.

It caches the walk's geometry, not radiance. It is not a glow cache.

## Why one bit per slot is enough

Every input to the walk is either global to the frame or per light, so
validity is decided on the CPU, once per frame and per shadow slot. No
per-pixel keys exist anywhere.

| Input | Granularity | Key |
|---|---|---|
| inverse view-projection, camera position, near and far planes | frame | `FrameKey::camera`, as exact bits |
| haze target size, depth-buffer size | frame | `FrameKey::camera` |
| opaque depth buffer | frame | `FrameKey::depth`, the identity of every opaque draw that wrote it |
| shadow projection and map contents | per slot | `SlotKey::shadow`, the `ShadowCacheKey` that decides whether the map is redrawn |
| range, field cosine, lens distance, wash | per slot | `SlotKey`, as raw bits: `beam_span` reads them and the shadow key only carries derived forms |
| whether the cone scatters, whether it has a gobo | per slot | `SlotKey` |
| the slot's screen rect in cache blocks | per slot | `SlotKey::rect`, because the region's addressing depends on it |

A camera move or a change to the depth inputs makes every region stale. A
moving head changes only its own slot's key. A dimmer or colour change
changes nothing.

## Why shadow slot is the key

The haze kernels iterate lights by light-index id `li`, which is rebuilt
and re-sorted every frame. `li` cannot address the cache.

`assign_shadow_slots` (`shadow.rs`) keeps residents in their slot unless a
challenger beats them by a 1.25× margin, and `assign_cached_slots` keeps
them when shadows are cached. Only lights with a slot do the walk. So the
slot number is the stable id, and `light_rest[li].shadow_slot` maps `li` to
it in the shader.

Slot eviction is a key change, which invalidates the slot. No extra logic.

## Layout

The kernels run 8×4 workgroups, one pixel per invocation, and a cache block
is one workgroup. The cache is active only at native haze resolution, where
an 8×4 block lies inside one 8-pixel light-index tile, so every lane of a
block walks the same light list.

Two tiers:

1. **Header.** Two `u32` words per (block, slot), one bit per lane. The
   first word's bit is set when the traversal clipped the pixel's ray out
   of the shadow frustum and integrated the whole span directly. The
   second's is set when it walked the map, found everything lit and
   integrated the whole span through its running sum. They are kept apart
   because the two round differently.
2. **Payload table.** Only for pixels that are not whole-lit. A hashed table
   of entries: the slot's generation, then count, slot and lane, then
   `CACHE_K = 8` interval pairs as raw `f32` bits. The table has 2^21
   entries (144 MiB), or as many as one storage binding of the device
   holds. A claim word per entry is atomic, so two pairs landing on the
   same free way in one frame cannot interleave.

Intervals stay `f32`. Quantising them would move quadrature endpoints and
forfeit bit-exactness.

A pair with more than `CACHE_K` intervals is not stored, and neither is a
pair whose hash bucket is full. Such a pair traverses on every frame, which
is slower but exact.

Header regions: each resident slot gets a contiguous region sized to its
screen rect in blocks, so a block's header is
`base + (by − y0) × w + (bx − x0)`. Regions are first-fit allocated
(`RegionAllocator`) and survive as long as the slot's key does. When the
pool cannot fit a new region, everything is compacted and every slot takes
one miss frame.

A slot's write generation is bumped on every write frame and stored in each
payload entry, so an entry written under an older key is never replayed.

## Modes

`IntervalCacheState::plan` gives each slot one of three modes per frame:

- **Off (0).** Traverse, never touch the pool.
- **Read (1).** The region holds this key's output and the frame key
  matches the previous frame's: replay it.
- **Write (2).** Traverse and store.

Writes are gated on the frame key having settled. A frame whose camera or
depth differs from the previous frame's traverses without writing, so a
continuous orbit never pays for stores it could not reuse. The first still
frame writes and the second reads. A per-slot change on a still camera (a
moving head) writes immediately.

## Shader path

`beam_scatter_cached` (`shaders/haze_cache.wgsl`) wraps the traversal:

```
no shadow slot, or mode off and no quad sharing -> plain traversal
read:
    header bit set  -> lit_interval over the whole span
    payload found   -> lit_interval over the stored intervals, in the traversal's order
    else            -> traverse
write:
    record the traversal's intervals
    publish the block's two header words
    if not whole-lit and count <= CACHE_K -> store the payload
    integrate from the recorded intervals
```

The traversal sums the two frustum tails `[full.x, span.x]` and
`[span.y, full.y]` first, then each merged run in turn. They are stored as
intervals 0 and 1, and the replay makes exactly those `lit_interval` calls
in that order and expression shape.

Exactness is verified by pixels, never assumed: under Metal fast-math a
change in expression shape alone changes pixels. The transition tests in
`gpu/tests/haze.rs` compare a renderer carrying haze state against one that
has forgotten it.

## What this does not do

- It does not touch scene shading.
- It does not reduce the quadrature.
- It does nothing while the camera moves.
