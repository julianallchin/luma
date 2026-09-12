// Residual compaction of the cached deterministic kernel
// (SESSION-2026-09-10.md, "Residual compaction design"). Appended to
// `haze.wgsl` after `haze_cache.wgsl` when the device has subgroup ballots.
//
// The fused kernel (`compute_haze`) is bounded, per 8×4 subgroup and light,
// by its slowest lane: a subgroup whose 32 lanes are not all whole-lit runs
// the shared quadrature for none of them and the per-lane paths (single
// `lit_interval`, payload replay, shadow traversal) for each. Attribution on
// the 3 s close frame (run-20260911-compact, `LUMA_HAZE_WORK_COUNTS=3`) put
// 22.6 % of subgroup-lights in that residual with 3.02 M lanes, 73 % of them
// whole-lit lanes that merely lost sharing. This file splits the work:
//
// 1. `fill_cache` (8×4, only on frames with a mode-2 slot): the traversal
//    alone for mode-2 slots, writing the header ballots and payload exactly
//    as the fused kernel's write path does, integrating nothing.
// 2. `classify_residual` (8×4, every compact frame): the fused kernel's
//    decision per (block, light) — span, header bits, apex test, payload
//    lookup — ending in one ballot. A subgroup-uniform whole-lit light stores
//    mask 0; any other stores mask = active lanes and a base index, and every
//    active lane appends `{pixel | light}` to the residual list at
//    base + prefix, plus an index into one of three type segments.
// 3. `compute_residual` (1D, 64 lanes, one indirect dispatch per segment):
//    each entry calls exactly the function the fused kernel would have
//    (`lit_interval`, `cache_replay`, `beam_shadow_integral`) and stores its
//    RGB as raw f32.
// 4. `compute_haze_hot` (8×4): mask 0 → `quad_scatter`; otherwise lanes in
//    the mask gather their stored RGB, other lanes add zero. The per-pixel sum
//    stays `deterministic += term` in light order.
//
// The hot kernel never recomputes a ballot: mask and base are read back from
// what classify stored, so a fast-math flip between pipelines cannot misalign
// a gather. Dense slot ids keep the (block, slot) planes at live-slot size.
struct CompactParams {
    // x: dense slot stride of the planes, y: residual list capacity, z: blocks per row,
    // w: segment id of this residual dispatch (0 single, 1 payload, 2 traverse).
    params: vec4<u32>,
    // xyz: segment offsets into the work list.
    seg_off: vec4<u32>,
    // xyz: segment capacities.
    seg_cap: vec4<u32>,
    // xyz: lanes per workgroup of each segment's compute dispatch.
    seg_lanes: vec4<u32>,
    // xy: the residual render target's size, when the residual runs as a
    // point-list fragment pass; one point per entry.
    point_target: vec4<u32>,
    // Shadow slot → dense id (0xFFFFFFFF: no resident light), 4 per vector.
    dense: array<vec4<u32>, 128>,
    // Dense id → this frame's light-index id. Light ids reshuffle every
    // frame; list entries carry the dense id, which is sticky per slot.
    slot_light: array<vec4<u32>, 64>,
    // One bit per shadow slot: its classification is rebuilt this frame.
    // A clean slot keeps its planes and its list entries from the frame
    // that classified it.
    dirty: array<vec4<u32>, 4>,
    // The same in light-index id space, laid out like a tile's mask words:
    // [0, 4) the classify set, [4, 8) the fill (write-mode) set. A block
    // whose tile mask misses the set has nothing to do and leaves at once.
    dirty_lights: array<vec4<u32>, 8>,
};
@group(2) @binding(8) var<uniform> compact: CompactParams;
// Two words per (block, dense slot): residual mask, list base.
@group(2) @binding(9) var<storage, read_write> resid_planes: array<u32>;
// Residual entries: pixel index (22 bits) | dense slot << 22 | kind << 30.
@group(2) @binding(10) var<storage, read_write> resid_list: array<u32>;
// Three raw f32 per entry, written by the residual dispatch.
@group(2) @binding(11) var<storage, read_write> resid_rgb: array<f32>;
// Per-segment index lists into `resid_list`.
@group(2) @binding(12) var<storage, read_write> resid_work: array<u32>;
// [0..3) segment counts, [3] list count, [4] overflow flag.
@group(2) @binding(13) var<storage, read_write> resid_counters: array<atomic<u32>>;
// Per entry, the payload table entry a replay lane found at classify time,
// so the residual replays without a second hash lookup.
@group(2) @binding(15) var<storage, read_write> resid_aux: array<u32>;
// Indirect arguments, eight words per segment: [workgroups, 1, 1, 0] for the
// compute dispatch and [6, tiles, 0, 0] for the tiled draw (one 64×64 tile
// of the scratch target per instance); the constant words are preset by the
// CPU.
@group(2) @binding(14) var<storage, read_write> resid_args: array<atomic<u32>>;

const RESID_SEG_LANES: u32 = 64u;
// The residual pipeline is specialised per segment so the single-call and
// replay kernels do not carry the traversal's register footprint (one
// function, one occupancy): 0 single, 1 payload, 2 traverse.
override RESID_KIND: u32 = 0u;
// Lanes per workgroup of the plain residual dispatch; the traversal segment
// runs narrower so its few long entries spread over more cores. The counted
// variant keeps `RESID_SEG_LANES` for its reduction arrays.
override RESID_LANES: u32 = 64u;
// Planes word for a light no lane of the block can see: the hot pass skips
// it without building a span.
const RESID_EMPTY: u32 = 0xFFFFFFFFu;
// Side of one scratch-target tile in the fragment residual; a tile holds
// 4096 entries and is drawn as one instance, so fragment quads are full.
const RESID_TILE: u32 = 64u;

// The fill and classify passes build the pixel's ray only once a light
// needs it: on an incremental frame few tiles hold a dirty light, and the
// ray (four depth loads, two matrix products) and the quad shuffles are the
// bulk of a tile's cost otherwise.
var<private> lazy_ray: SceneRay;
var<private> lazy_frag: vec2<f32>;
var<private> lazy_ray_ready: bool;
var<private> lazy_quad_ready: bool;

fn lazy_scene_ray() -> SceneRay {
    if !lazy_ray_ready {
        lazy_ray = scene_ray(lazy_frag);
        lazy_ray_ready = true;
    }
    return lazy_ray;
}

// Subgroup-uniform call sites only: `quad_prepare` shuffles.
fn lazy_quad_prepare() {
    if !lazy_quad_ready {
        quad_prepare(lazy_frag, lazy_scene_ray());
        lazy_quad_ready = true;
    }
}

fn compact_dense(slot: u32) -> u32 {
    return compact.dense[slot >> 2u][slot & 3u];
}
fn compact_light(dense: u32) -> u32 {
    return compact.slot_light[dense >> 2u][dense & 3u];
}
// Whether this tile's light list intersects the dirty-light set `set`
// (0 classify, 1 fill); `cursor.base` already points at the plane in use.
fn compact_tile_dirty(cursor: LightCursor, which: u32) -> bool {
    var hit = 0u;
    for (var word = 0u; word < LIGHT_INDEX_WORDS; word += 1u) {
        hit |= light_index_masks[cursor.base + word] & compact.dirty_lights[which * 4u + (word >> 2u)][word & 3u];
    }
    return hit != 0u;
}
fn compact_dirty(slot: u32) -> bool {
    return ((compact.dirty[slot >> 7u][(slot >> 5u) & 3u] >> (slot & 31u)) & 1u) == 1u;
}

// The fused kernel's slot resolution: mode, generation, header word index and
// payload key for this block, or mode 0 when the block lies outside the
// slot's rect.
struct SlotLookup {
    mode: u32,
    gen: u32,
    header: u32,
    key: u32,
    slot: u32,
};

fn compact_slot(li: u32) -> SlotLookup {
    var out = SlotLookup(0u, 0u, 0u, 0u, 0u);
    let slot = u32(max(light_rest[li].shadow_slot, 0.0));
    out.slot = slot;
    let entry = cache_params.slots[slot];
    let x0 = entry.y & 0xFFFFu;
    let y0 = entry.y >> 16u;
    let w = entry.z & 0xFFFFu;
    let h = entry.z >> 16u;
    if (entry.w & 0xFFu) != 0u && cache_bx >= x0 && cache_bx - x0 < w && cache_by >= y0 && cache_by - y0 < h {
        out.mode = entry.w & 0xFFu;
        out.gen = entry.w >> 8u;
        out.header = entry.x + (cache_by - y0) * w + (cache_bx - x0);
        out.key = slot << 5u | cache_lane;
    }
    return out;
}

// --- 1. fill ---------------------------------------------------------------

// The fused kernel's mode-2 path without its quadrature: the traversal
// records the call list, the ballots become the header words, the payload
// is stored. `FILL_ONLY` makes `lit_counted` return zero after recording.
fn fill_light(li: u32) {
    if light_rest[li].haze_gain <= 0.0 { return; }
    if haze.shadow.x <= 0.0 || PROFILE_SKIP_NATIVE_SHADOWS { return; }
    let lookup = compact_slot(li);
    if lookup.mode != 2u { return; }
    let ray = lazy_scene_ray();
    let span = beam_span(li, ray);
    if span.y <= span.x || PROFILE_SKIP_NATIVE_INTEGRALS { return; }
    _ = beam_shadow_integral(li, ray, span);
    let direct = cache_count == 1u;
    let walked = cache_count == 3u && all(cache_intervals[2] == span);
    let ballot_direct = cache_ballot(direct);
    let ballot_walked = cache_ballot(walked);
    if cache_elect() {
        cache_header[lookup.header * 2u] = ballot_direct;
        cache_header[lookup.header * 2u + 1u] = ballot_walked;
    }
    if !direct && !walked && cache_count <= CACHE_K { cache_store(cache_block, lookup.key, lookup.gen); }
}

@compute @workgroup_size(HAZE_GROUP_X, HAZE_GROUP_Y)
fn fill_cache(@builtin(global_invocation_id) pixel: vec3<u32>,
              @builtin(local_invocation_index) lane: u32) {
    if any(pixel.xy >= textureDimensions(haze_exact_out)) { return; }
    cache_locate(pixel.xy, lane);
    let frag = vec2<f32>(pixel.xy) + 0.5;
    lazy_frag = frag;
    lazy_ray_ready = false;
    lazy_quad_ready = false;
    if haze.params.y < 0.001 || haze.tiles.z <= 0.5 { return; }
    var cursor = lights_along(frag * haze.tiles.xy);
    cursor.base += light_index_params.grid.x * light_index_params.grid.y * LIGHT_INDEX_WORDS;
    cursor.bits = light_index_masks[cursor.base + cursor.word];
    if !compact_tile_dirty(cursor, 1u) { return; }
    var li = 0u;
    while light_index_next(&cursor, &li) {
        fill_light(li);
    }
}

// --- 2. classify -----------------------------------------------------------

// Every in-bounds lane of the subgroup reaches every subgroup operation
// here: the only early return is on a light-uniform condition.
fn classify_light(li: u32, pixel_index: u32) {
    if HAZE_WORK_COUNTS { haze_work[0] += 1u; }
    if light_rest[li].haze_gain <= 0.0 { return; }
    let lookup = compact_slot(li);
    if !compact_dirty(lookup.slot) { return; }
    lazy_quad_prepare();
    let ray = lazy_ray;
    let dense = compact_dense(lookup.slot);
    let plane = (cache_block * compact.params.x + dense) * 2u;
    let cached = lookup.mode != 0u && haze.shadow.x > 0.0 && !PROFILE_SKIP_NATIVE_SHADOWS
        && !PROFILE_SKIP_NATIVE_INTEGRALS;
    var h0 = 0u;
    var h1 = 0u;
    var whole = false;
    // A write-mode slot is filled here: the fused kernel's mode-2 path
    // (traversal, header ballots, payload store) with its results kept in
    // registers, so the classification below needs no header load and no
    // payload lookup for it.
    var filled = CACHE_NONE;
    var fill_span = vec2<f32>(0.0);
    var fill_live = false;
    if cached && lookup.mode == 2u {
        fill_span = beam_span(li, ray);
        fill_live = fill_span.y > fill_span.x;
        var direct = false;
        var walked = false;
        if fill_live {
            _ = beam_shadow_integral(li, ray, fill_span);
            direct = cache_count == 1u;
            walked = cache_count == 3u && all(cache_intervals[2] == fill_span);
        }
        h0 = subgroupBallot(fill_live && direct).x;
        h1 = subgroupBallot(fill_live && walked).x;
        if cache_lane == 0u {
            cache_header[lookup.header * 2u] = h0;
            cache_header[lookup.header * 2u + 1u] = h1;
        }
        if fill_live && !direct && !walked && cache_count <= CACHE_K {
            filled = cache_store_index(cache_block, lookup.key, lookup.gen);
        }
    } else if cached {
        h0 = cache_header[lookup.header * 2u];
        h1 = cache_header[lookup.header * 2u + 1u];
    }
    if cached {
        // A header bit is only ever set for a lane whose span was live when
        // the slot was written, and the slot is valid only while every input
        // to that span is unchanged: the whole-lit test needs no span here.
        whole = QUADSHARE != 0u && (((h0 | h1) >> cache_lane) & 1u) == 1u;
        if whole && QUAD_APEX_PX > 0.0 {
            let oc = haze.camera_pos.xyz - light_core[li].position;
            let b = dot(oc, ray.dir);
            let oo = dot(oc, oc);
            whole = oo - b * b >= quad_apex * oo;
        }
    }
    let bits = subgroupBallot(whole).x;
    if QUADSHARE != 0u && QUAD_UNIFORM != 0u && bits == 0xFFFFFFFFu {
        // Subgroup-uniform whole-lit: the hot pass shares this light.
        if HAZE_WORK_COUNTS { haze_work[1] += 1u; }
        if cache_lane == 0u {
            resid_planes[plane] = 0u;
            resid_planes[plane + 1u] = 0u;
        }
        return;
    }
    var span = fill_span;
    if !(cached && lookup.mode == 2u) { span = beam_span(li, ray); }
    let live = span.y > span.x && !PROFILE_SKIP_NATIVE_INTEGRALS;
    if HAZE_WORK_COUNTS && live { haze_work[1] += 1u; }
    // 0: single direct, 1: single walked, 2: payload replay, 3: traversal.
    var kind = 3u;
    var found = CACHE_NONE;
    if live && cached {
        let direct = ((h0 >> cache_lane) & 1u) == 1u;
        let walked = ((h1 >> cache_lane) & 1u) == 1u;
        if direct && (cache_params.flags.x & 2u) == 0u {
            kind = 0u;
        } else if walked || (direct && (cache_params.flags.x & 2u) != 0u) {
            kind = 1u;
        } else {
            if lookup.mode == 2u {
                found = filled;
            } else if (cache_params.flags.x & 1u) == 1u {
                found = cache_find(cache_block, lookup.key, lookup.gen);
            }
            if found != CACHE_NONE {
                kind = 2u;
            } else if (cache_params.flags.x & 4u) != 0u {
                kind = 0u;
            }
        }
    }
    let r = subgroupBallot(live).x;
    let n = countOneBits(r);
    var base = RESID_EMPTY;
    if n > 0u && cache_lane == 0u {
        base = atomicAdd(&resid_counters[3], n);
        if base + n > compact.params.y { atomicMax(&resid_counters[4], 1u); }
    }
    base = subgroupBroadcast(base, 0u);
    if cache_lane == 0u {
        resid_planes[plane] = r;
        resid_planes[plane + 1u] = base;
    }
    if n == 0u { return; }
    let in_r = ((r >> cache_lane) & 1u) == 1u;
    let index = base + countOneBits(r & ((1u << cache_lane) - 1u));
    if in_r && index < compact.params.y {
        resid_list[index] = pixel_index | (dense << 22u) | (kind << 30u);
        if kind == 2u { resid_aux[index] = found; }
    }
    let seg = select(select(0u, 1u, kind == 2u), 2u, kind == 3u);
    for (var t = 0u; t < 3u; t += 1u) {
        let rt = subgroupBallot(in_r && seg == t).x;
        let nt = countOneBits(rt);
        var wb = 0u;
        if nt > 0u && cache_lane == 0u {
            wb = atomicAdd(&resid_counters[t], nt);
            let cap = compact.seg_cap[t];
            if wb + nt > cap { atomicMax(&resid_counters[4], 1u); }
            let lanes = compact.seg_lanes[t];
            atomicMax(&resid_args[t * 8u], (min(wb + nt, cap) + lanes - 1u) / lanes);
            atomicMax(&resid_args[t * 8u + 5u], (min(wb + nt, cap) + RESID_TILE * RESID_TILE - 1u) / (RESID_TILE * RESID_TILE));
        }
        wb = subgroupBroadcast(wb, 0u);
        if ((rt >> cache_lane) & 1u) == 1u {
            let j = wb + countOneBits(rt & ((1u << cache_lane) - 1u));
            if j < compact.seg_cap[t] && index < compact.params.y {
                resid_work[compact.seg_off[t] + j] = index;
            }
        }
    }
}

fn classify_pixel(pixel: vec2<u32>, lane: u32) {
    cache_locate(pixel, lane);
    let frag = vec2<f32>(pixel) + 0.5;
    lazy_frag = frag;
    lazy_ray_ready = false;
    lazy_quad_ready = false;
    if haze.params.y < 0.001 || haze.tiles.z <= 0.5 { return; }
    let pixel_index = pixel.y * u32(haze.transport.w) + pixel.x;
    var cursor = lights_along(frag * haze.tiles.xy);
    cursor.base += light_index_params.grid.x * light_index_params.grid.y * LIGHT_INDEX_WORDS;
    cursor.bits = light_index_masks[cursor.base + cursor.word];
    if !compact_tile_dirty(cursor, 0u) { return; }
    var li = 0u;
    while light_index_next(&cursor, &li) {
        classify_light(li, pixel_index);
    }
}

@compute @workgroup_size(HAZE_GROUP_X, HAZE_GROUP_Y)
fn classify_residual(@builtin(global_invocation_id) pixel: vec3<u32>,
                     @builtin(local_invocation_index) lane: u32) {
    if any(pixel.xy >= textureDimensions(haze_exact_out)) { return; }
    classify_pixel(pixel.xy, lane);
}

@compute @workgroup_size(HAZE_GROUP_X, HAZE_GROUP_Y)
fn classify_residual_counted(@builtin(global_invocation_id) pixel: vec3<u32>,
                             @builtin(workgroup_id) group: vec3<u32>,
                             @builtin(local_invocation_index) lane: u32) {
    if all(pixel.xy < textureDimensions(haze_exact_out)) {
        classify_pixel(pixel.xy, lane);
    }
    record_haze_work(group, lane);
}

// --- 3. residual -----------------------------------------------------------

fn residual_entry(gid: u32) {
    let seg = RESID_KIND;
    let count = min(atomicLoad(&resid_counters[seg]), compact.seg_cap[seg]);
    if gid >= count { return; }
    let index = resid_work[compact.seg_off[seg] + gid];
    let entry = resid_list[index];
    let pixel_index = entry & 0x3FFFFFu;
    let li = compact_light((entry >> 22u) & 0xFFu);
    let kind = entry >> 30u;
    let width = u32(haze.transport.w);
    let pixel = vec2<u32>(pixel_index % width, pixel_index / width);
    cache_locate(pixel, (pixel.y & 3u) * 8u + (pixel.x & 7u));
    let ray = scene_ray(vec2<f32>(pixel) + 0.5);
    let span = beam_span(li, ray);
    var result = vec3<f32>(0.0);
    if seg == 0u {
        if kind == 1u {
            // The traversal's `sum` started from two empty tails and took
            // one run: the same product added to a zero the compiler cannot
            // fold away (`haze.tiles.w` is always zero).
            var sum = vec3<f32>(haze.tiles.w);
            sum += lit_interval(li, ray, span.x, span.y);
            result = sum;
        } else {
            result = lit_interval(li, ray, span.x, span.y);
        }
    } else if seg == 1u {
        result = cache_replay(li, ray, resid_aux[index]);
    } else {
        result = beam_shadow_integral(li, ray, span);
    }
    resid_rgb[index * 3u] = result.x;
    resid_rgb[index * 3u + 1u] = result.y;
    resid_rgb[index * 3u + 2u] = result.z;
}

@compute @workgroup_size(RESID_LANES)
fn compute_residual(@builtin(global_invocation_id) gid: vec3<u32>) {
    residual_entry(gid.x);
}

// The same entries as a draw: entry e lives at pixel (e / 4096 → tile,
// e % 4096 → 64×64 position) of a scratch target, one quad per tile, so the
// residual quadrature runs on the render lane beside the far-field grid
// chain instead of ahead of it in the serial compute lane.
struct ResidualVertex {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_residual(@builtin(vertex_index) vi: u32, @builtin(instance_index) tile: u32) -> ResidualVertex {
    let tiles_x = compact.point_target.x / RESID_TILE;
    let origin = vec2<f32>(f32(tile % tiles_x), f32(tile / tiles_x)) * f32(RESID_TILE);
    // Two triangles: 0 1 2, 2 1 3 over the tile's corners.
    var corners = array<u32, 6>(0u, 1u, 2u, 2u, 1u, 3u);
    let corner = corners[vi];
    let offset = vec2<f32>(f32(corner & 1u), f32(corner >> 1u)) * f32(RESID_TILE);
    let uv = (origin + offset) / vec2<f32>(compact.point_target.xy);
    return ResidualVertex(vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0));
}

@fragment
fn fs_residual(vertex: ResidualVertex) -> @location(0) vec4<f32> {
    let p = vec2<u32>(vertex.position.xy);
    let tiles_x = compact.point_target.x / RESID_TILE;
    let tile = (p.y / RESID_TILE) * tiles_x + p.x / RESID_TILE;
    let index = tile * RESID_TILE * RESID_TILE + (p.y % RESID_TILE) * RESID_TILE + (p.x % RESID_TILE);
    residual_entry(index);
    return vec4<f32>(0.0);
}

var<workgroup> resid_sum_a: array<vec4<u32>, RESID_SEG_LANES>;
var<workgroup> resid_sum_b: array<vec4<u32>, RESID_SEG_LANES>;

@compute @workgroup_size(RESID_SEG_LANES)
fn compute_residual_counted(@builtin(global_invocation_id) gid: vec3<u32>,
                            @builtin(workgroup_id) group: vec3<u32>,
                            @builtin(local_invocation_index) lane: u32) {
    residual_entry(gid.x);
    resid_sum_a[lane] = vec4<u32>(haze_work[0], haze_work[1], haze_work[2], haze_work[3]);
    resid_sum_b[lane] = vec4<u32>(haze_work[4], haze_work[5], haze_work[6], haze_work[7]);
    workgroupBarrier();
    for (var stride = RESID_SEG_LANES / 2u; stride > 0u; stride /= 2u) {
        if lane < stride {
            resid_sum_a[lane] += resid_sum_a[lane + stride];
            resid_sum_b[lane] += resid_sum_b[lane + stride];
        }
        workgroupBarrier();
    }
    if lane == 0u {
        // Segment records follow one another: segment t's workgroup g lands
        // at (seg_off[t] / 64 + g). Sums only; the maxima slots stay zero.
        let base = (compact.seg_off[compact.params.w] / RESID_SEG_LANES + group.x) * 16u;
        for (var component = 0u; component < 4u; component += 1u) {
            work_counts[base + component] = resid_sum_a[0][component];
            work_counts[base + 4u + component] = resid_sum_b[0][component];
            work_counts[base + 8u + component] = 0u;
            work_counts[base + 12u + component] = 0u;
        }
    }
}

// --- 4. hot ----------------------------------------------------------------

// `beam_scatter_cached` up to its ballot; the ballot's answer is read back
// from the classify pass.
fn beam_scatter_hot(li: u32, ray: SceneRay) -> vec3<f32> {
    let haze_gain = light_rest[li].haze_gain;
    if haze_gain <= 0.0 {
        return vec3<f32>(0.0);
    }
    if PROFILE_SKIP_NATIVE_INTEGRALS { return vec3<f32>(0.0); }
    let lookup = compact_slot(li);
    let plane = (cache_block * compact.params.x + compact_dense(lookup.slot)) * 2u;
    let r = resid_planes[plane];
    if r == 0u {
        // Uniform whole-lit, or no lane sees the light at all.
        if resid_planes[plane + 1u] == RESID_EMPTY { return vec3<f32>(0.0); }
        let span = beam_span(li, ray);
        if !subgroupAny(span.y > span.x) { return vec3<f32>(0.0); }
        if HAZE_WORK_COUNTS { haze_work[7] += 1u; }
        if QUAD_DIAG == 1u { return vec3<f32>(0.0); }
        return quad_scatter(li, ray, span);
    }
    if ((r >> cache_lane) & 1u) == 0u { return vec3<f32>(0.0); }
    let index = resid_planes[plane + 1u] + countOneBits(r & ((1u << cache_lane) - 1u));
    if index >= compact.params.y { return vec3<f32>(0.0); }
    return vec3<f32>(resid_rgb[index * 3u], resid_rgb[index * 3u + 1u], resid_rgb[index * 3u + 2u]);
}

fn haze_at_hot(frag: vec4<f32>) -> HazeOutput {
    var ray = scene_ray(frag.xy);
    quad_prepare(frag.xy, ray);
    let weight = haze.tuning.y;
    let density = haze.params.y;

    if density < 0.001 {
        return HazeOutput(vec4<f32>(0.0, 0.0, 0.0, ray.view_depth * weight), vec4<f32>(0.0));
    }

    var deterministic = vec3<f32>(0.0);
    let include_shared = GRID_FOG && haze.tiles.z > 1.5;

    if haze.tiles.z > 0.5 {
        var cursor = lights_along(frag.xy * haze.tiles.xy);
        if GRID_FOG {
            cursor.base += light_index_params.grid.x * light_index_params.grid.y * LIGHT_INDEX_WORDS;
            cursor.bits = light_index_masks[cursor.base + cursor.word];
        }
        var li = 0u;
        while light_index_next(&cursor, &li) {
            deterministic += beam_scatter_hot(li, ray);
        }
    }

    if include_shared {
        let size = vec3<f32>(textureDimensions(fog_grid));
        let uv = clamp(frag.xy / vec2<f32>(haze.transport.w, haze.transport.z), 0.5 / size.xy, 1.0 - 0.5 / size.xy);
        let span = ray.fog_span;
        let radial = sqrt(clamp((ray.hit_dist - span.x) / max(span.y - span.x, 1e-5), 0.0, 1.0));
        let z = (radial * (size.z - 1.0) + 0.5) / size.z;
        deterministic += textureSampleLevel(fog_grid, haze_noise_sampler, vec3<f32>(uv, z), 0.0).rgb * 256.0;
    }

    let depth_weight = select(weight, select(0.0, 1.0, include_shared), GRID_FOG);
    return HazeOutput(vec4<f32>(deterministic, ray.view_depth * depth_weight), vec4<f32>(0.0));
}

@compute @workgroup_size(HAZE_GROUP_X, HAZE_GROUP_Y)
fn compute_haze_hot(@builtin(global_invocation_id) pixel: vec3<u32>,
                    @builtin(local_invocation_index) lane: u32) {
    if any(pixel.xy >= textureDimensions(haze_exact_out)) { return; }
    cache_locate(pixel.xy, lane);
    let result = haze_at_hot(vec4<f32>(vec2<f32>(pixel.xy) + 0.5, 0.0, 1.0));
    textureStore(haze_exact_out, vec2<i32>(pixel.xy), result.exact);
    textureStore(haze_sampled_out, vec2<i32>(pixel.xy), result.sampled);
}

@compute @workgroup_size(HAZE_GROUP_X, HAZE_GROUP_Y)
fn compute_haze_hot_counted(@builtin(global_invocation_id) pixel: vec3<u32>,
                            @builtin(workgroup_id) group: vec3<u32>,
                            @builtin(local_invocation_index) lane: u32) {
    if all(pixel.xy < textureDimensions(haze_exact_out)) {
        cache_locate(pixel.xy, lane);
        let result = haze_at_hot(vec4<f32>(vec2<f32>(pixel.xy) + 0.5, 0.0, 1.0));
        textureStore(haze_exact_out, vec2<i32>(pixel.xy), result.exact);
        textureStore(haze_sampled_out, vec2<i32>(pixel.xy), result.sampled);
    }
    record_haze_work(group, lane);
}
