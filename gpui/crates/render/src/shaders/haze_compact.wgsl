// Residual compaction of the cached compute haze kernel. Appended to
// `haze.wgsl` after `haze_cache.wgsl`.
//
// The fused kernel (`compute_haze`) is bounded, per 8×4 workgroup and light,
// by its slowest lane: a block whose 32 lanes are not all whole-lit runs the
// shared quadrature for none of them and the per-lane paths (single
// `lit_interval`, payload replay, shadow traversal) for each. Attribution on
// the 3 s close frame (run-20260911-compact) put 22.6 % of block-lights in
// that residual with 3.02 M lanes, 73 % of them whole-lit lanes that merely
// lost sharing. This file splits the work:
//
// 1. `classify_residual` (8×4, on frames where some slot changed): the fused
//    kernel's decision per (block, light) — span, header bits, apex test,
//    payload lookup — ending in one ballot. A write-mode slot is filled here
//    first, exactly as the fused kernel's write path fills it. A uniform
//    whole-lit light of a one-subgroup block stores mask 0; any other stores
//    mask = live lanes and a base index, and every live lane appends
//    `{pixel | light}` to the residual list at base + prefix with its kind.
//    Lights without a shadow map are not classified.
// 2. `prepare_residual_work` and `scatter_residual_work` turn the list into
//    three contiguous typed work ranges and their indirect dispatches.
// 3. `compute_residual` (1D, one indirect dispatch per kind): each entry calls
//    exactly the function the fused kernel would have (`lit_interval`,
//    `cache_replay`, `beam_shadow_integral`) and stores its RGB as raw f32.
// 4. `compute_haze_hot` (8×4): mask 0 → `quad_scatter` (each lane's own
//    whole-lit interval when this pipeline's block is not one subgroup);
//    otherwise lanes in the mask gather their stored RGB, other lanes add
//    zero; a light without a shadow map integrates in place. The per-pixel
//    sum stays `deterministic += term` in light order.
//
// A slot keeps its planes and list entries while its interval-cache
// generation holds, so a still frame with no slot change runs only steps 3
// and 4. The hot kernel never recomputes a ballot: mask and base are read
// back from what classify stored, so codegen drift between pipelines cannot
// misalign a gather. Dense slot ids keep the (block, slot) planes at
// live-slot size.
struct CompactParams {
    // x: dense slot stride of the planes, y: residual list capacity,
    // z: blocks per row, w: maximum compute workgroups per dimension (the row
    // stride of a large 2D indirect dispatch).
    params: vec4<u32>,
    // Shadow slot → dense id (0xFFFFFFFF: no resident light), 4 per vector.
    dense: array<vec4<u32>, 128>,
    // Dense id → this frame's light-index id. Light ids reshuffle every
    // frame; list entries carry the dense id, which is sticky per slot.
    slot_light: array<vec4<u32>, 128>,
    // One bit per shadow slot: its classification is rebuilt this frame.
    dirty: array<vec4<u32>, 4>,
    // The same set in light-index id space, laid out like a tile's mask
    // words. A block whose tile mask misses the set leaves at once.
    dirty_lights: array<vec4<u32>, 4>,
};
@group(2) @binding(8) var<uniform> compact: CompactParams;
// Two words per (block, dense slot): residual mask, list base.
@group(2) @binding(9) var<storage, read_write> resid_planes: array<u32>;
// Residual entries: pixel index (22 bits) | dense slot << 22 | walked << 31.
// The walked bit avoids an aux load in the single-interval residual kernel.
@group(2) @binding(10) var<storage, read_write> resid_list: array<u32>;
// Three raw f32 per entry, written by the residual dispatch.
@group(2) @binding(11) var<storage, read_write> resid_rgb: array<f32>;
// Contiguous typed index lists into `resid_list`, one range per kind.
@group(2) @binding(12) var<storage, read_write> resid_work: array<u32>;
// [0..3) entries per kind (single, payload, traverse), [3] list count,
// [4] overflow flag, [5..8) each kind's offset into `resid_work`,
// [8..11) scatter cursors.
@group(2) @binding(13) var<storage, read_write> resid_counters: array<atomic<u32>>;
// Per entry, its kind, or for a payload replay the table entry classify
// found, so the residual replays without a second hash lookup.
@group(2) @binding(15) var<storage, read_write> resid_aux: array<u32>;
// Indirect arguments: [0..12) one [x, y, 1, 0] dispatch per kind,
// [12..15) the scatter dispatch, then the compact hot dispatch, [15..18)
// the fused fallback, [18..20) the block grid dimensions.
@group(2) @binding(14) var<storage, read_write> resid_args: array<atomic<u32>>;

// The residual pipeline is specialised per kind so the single-call and
// replay kernels do not carry the traversal's register footprint (one
// function, one occupancy): 0 single, 1 payload, 2 traverse.
override RESID_KIND: u32 = 0u;
// Lanes per workgroup of the residual dispatch; the traversal kind runs
// narrower so its few long entries spread over more cores.
override RESID_LANES: u32 = 64u;
const RESID_DIRECT: u32 = 0x7FFFFFFDu;
const RESID_WALKED: u32 = 0x7FFFFFFEu;
const RESID_TRAVERSE: u32 = 0x7FFFFFFFu;
// Planes word for a light no lane of the block can see: the hot pass skips
// it without building a span.
const RESID_EMPTY: u32 = 0xFFFFFFFFu;
const RESID_SCATTER_LANES: u32 = 128u;

// Classify builds the pixel's ray only once a light needs it: on an
// incremental frame few tiles hold a dirty light, and the ray (four depth
// loads, two matrix products) and the quad shuffles are the bulk of a tile's
// cost otherwise.
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

// Block-uniform call sites only: `quad_prepare` shuffles.
fn lazy_quad_prepare() {
    if !lazy_quad_ready {
        if block_one_subgroup { quad_prepare(lazy_frag, lazy_scene_ray()); }
        lazy_quad_ready = true;
    }
}

fn compact_dense(slot: u32) -> u32 {
    return compact.dense[slot >> 2u][slot & 3u];
}
fn compact_light(dense: u32) -> u32 {
    return compact.slot_light[dense >> 2u][dense & 3u];
}
// Whether this tile's light list intersects the dirty-light set;
// `cursor.base` already points at the plane in use.
fn compact_tile_dirty(cursor: LightCursor) -> bool {
    var hit = 0u;
    for (var word = 0u; word < LIGHT_INDEX_WORDS; word += 1u) {
        hit |= light_index_masks[cursor.base + word] & compact.dirty_lights[word >> 2u][word & 3u];
    }
    return hit != 0u;
}
fn compact_dirty(slot: u32) -> bool {
    return ((compact.dirty[slot >> 7u][(slot >> 5u) & 3u] >> (slot & 31u)) & 1u) == 1u;
}

// --- 1. classify -----------------------------------------------------------

// Every lane of the block reaches every block exchange here: the early
// returns are on light- and block-uniform conditions.
fn classify_light(li: u32, pixel_index: u32) {
    // Shadows are on (compaction runs only on cached frames); a light without
    // a map has no slot to classify and integrates in the hot pass.
    if light_rest[li].haze_gain <= 0.0 || light_rest[li].shadow_slot < 0.0 { return; }
    let slot = u32(light_rest[li].shadow_slot);
    if !compact_dirty(slot) { return; }
    let lookup = cache_slot(li);
    lazy_quad_prepare();
    let ray = lazy_scene_ray();
    let dense = compact_dense(slot);
    let plane = (cache_block * compact.params.x + dense) * 2u;
    // The fused kernel's decision, lane for lane: whole-lit lanes from the
    // header in read mode, else from the traversal recorded here (FILL_ONLY),
    // which a write-mode slot also stores, keeping its payload entry in a
    // register so the classification below needs no lookup for it.
    var span = vec2<f32>(0.0);
    var direct = false;
    var walked = false;
    var filled = CACHE_NONE;
    if lookup.mode == 1u {
        // A header bit is only ever set for a lane whose span was live when
        // the slot was written, and the slot is valid only while every input
        // to that span is unchanged: the whole-lit test needs no span here.
        direct = ((cache_header[lookup.header * 2u] >> cache_lane) & 1u) == 1u;
        walked = ((cache_header[lookup.header * 2u + 1u] >> cache_lane) & 1u) == 1u;
    } else {
        span = lane_span(li, ray);
        if span.y > span.x {
            _ = beam_shadow_integral(li, ray, span);
            direct = cache_count == 1u;
            walked = cache_count == 3u && all(cache_intervals[2] == span);
        }
    }
    if lookup.mode == 2u {
        let h0 = block_ballot(direct);
        let h1 = block_ballot(walked);
        if cache_lane == 0u {
            cache_header[lookup.header * 2u] = h0;
            cache_header[lookup.header * 2u + 1u] = h1;
        }
        if span.y > span.x && !direct && !walked && cache_count <= CACHE_K {
            filled = cache_store(cache_block, lookup.key, lookup.gen);
        }
    }
    if block_one_subgroup {
        let whole = (direct || walked) && light_rest[li].gobo < 0.5 && quad_apex_clear(li, ray);
        if subgroupBallot(whole).x == 0xFFFFFFFFu {
            if cache_lane == 0u {
                resid_planes[plane] = 0u;
                resid_planes[plane + 1u] = 0u;
            }
            return;
        }
    }
    if lookup.mode == 1u { span = lane_span(li, ray); }
    let live = span.y > span.x;
    // 0: single direct, 1: single walked, 2: payload replay, 3: traversal.
    var kind = 3u;
    var found = CACHE_NONE;
    if direct {
        kind = 0u;
    } else if walked {
        kind = 1u;
    } else if live {
        if lookup.mode == 2u {
            found = filled;
        } else if lookup.mode == 1u {
            found = cache_find(cache_block, lookup.key, lookup.gen);
        }
        if found != CACHE_NONE { kind = 2u; }
    }
    let r = block_ballot(live);
    let n = countOneBits(r);
    var base = RESID_EMPTY;
    if n > 0u && cache_lane == 0u {
        base = atomicAdd(&resid_counters[3], n);
        if base + n > compact.params.y { atomicMax(&resid_counters[4], 1u); }
    }
    if n > 0u { base = block_broadcast_first(base); }
    if cache_lane == 0u {
        resid_planes[plane] = r;
        resid_planes[plane + 1u] = base;
    }
    if n == 0u { return; }
    let in_r = ((r >> cache_lane) & 1u) == 1u;
    let index = base + countOneBits(r & ((1u << cache_lane) - 1u));
    if in_r && index < compact.params.y {
        let walked_bit = select(0u, 0x80000000u, kind == 1u);
        resid_list[index] = pixel_index | (dense << 22u) | walked_bit;
        if kind == 0u {
            resid_aux[index] = RESID_DIRECT;
        } else if kind == 1u {
            resid_aux[index] = RESID_WALKED;
        } else if kind == 2u {
            resid_aux[index] = found;
        } else {
            resid_aux[index] = RESID_TRAVERSE;
        }
    }
    let seg = select(select(0u, 1u, kind == 2u), 2u, kind == 3u);
    for (var t = 0u; t < 3u; t += 1u) {
        let nt = countOneBits(block_ballot(in_r && seg == t));
        if nt > 0u && cache_lane == 0u {
            atomicAdd(&resid_counters[t], nt);
        }
    }
}

@compute @workgroup_size(HAZE_BLOCK_LANES)
fn classify_residual(@builtin(workgroup_id) group: vec3<u32>,
                     @builtin(local_invocation_index) local: u32,
                     @builtin(subgroup_invocation_id) sub_lane: u32,
                     @builtin(num_subgroups) subgroups: u32) {
    let pixel = block_locate(group.xy, local, sub_lane, subgroups);
    let frag = vec2<f32>(pixel) + 0.5;
    lazy_frag = frag;
    lazy_ray_ready = false;
    lazy_quad_ready = false;
    if haze.params.y < 0.001 { return; }
    let pixel_index = pixel.y * u32(haze.transport.w) + pixel.x;
    var cursor = native_lights(frag);
    if !compact_tile_dirty(cursor) { return; }
    var li = 0u;
    while light_index_next(&cursor, &li) {
        classify_light(li, pixel_index);
    }
}

// --- 2. work ranges --------------------------------------------------------

var<workgroup> scatter_counts: array<atomic<u32>, 3>;
var<workgroup> scatter_bases: array<u32, 3>;

fn residual_kind(descriptor: u32) -> u32 {
    if descriptor == RESID_DIRECT || descriptor == RESID_WALKED { return 0u; }
    if descriptor == RESID_TRAVERSE { return 2u; }
    return 1u;
}

fn residual_lanes(kind: u32) -> u32 {
    return select(64u, 32u, kind == 2u);
}

// Each kind's range in `resid_work` and its indirect dispatch, from the
// counters classify produced in this submission; then the scatter dispatch.
@compute @workgroup_size(1)
fn prepare_residual_work() {
    let total = atomicLoad(&resid_counters[3]);
    let conserved = atomicLoad(&resid_counters[0])
        + atomicLoad(&resid_counters[1])
        + atomicLoad(&resid_counters[2]) == total;
    let overflow = atomicLoad(&resid_counters[4]) != 0u
        || total > compact.params.y || !conserved;
    if overflow { atomicMax(&resid_counters[4], 1u); }
    let columns = compact.params.w;
    var offset = 0u;
    for (var kind = 0u; kind < 3u; kind += 1u) {
        let count = select(atomicLoad(&resid_counters[kind]), 0u, overflow);
        atomicStore(&resid_counters[5u + kind], offset);
        atomicStore(&resid_counters[8u + kind], 0u);
        offset += count;
        let lanes = residual_lanes(kind);
        let groups = (count + lanes - 1u) / lanes;
        atomicStore(&resid_args[kind * 4u], min(groups, columns));
        atomicStore(&resid_args[kind * 4u + 1u], (groups + columns - 1u) / columns);
        atomicStore(&resid_args[kind * 4u + 2u], 1u);
    }
    let scatter_count = select(total, 0u, overflow);
    let scatter_groups = (scatter_count + RESID_SCATTER_LANES - 1u) / RESID_SCATTER_LANES;
    atomicStore(&resid_args[12], min(scatter_groups, columns));
    atomicStore(&resid_args[13], (scatter_groups + columns - 1u) / columns);
    atomicStore(&resid_args[14], 1u);
}

// Repack the append-only list into the three kind ranges.
@compute @workgroup_size(RESID_SCATTER_LANES)
fn scatter_residual_work(@builtin(workgroup_id) group: vec3<u32>,
                         @builtin(local_invocation_index) lane: u32) {
    if lane < 3u { atomicStore(&scatter_counts[lane], 0u); }
    workgroupBarrier();
    let flat_group = group.y * compact.params.w + group.x;
    let index = flat_group * RESID_SCATTER_LANES + lane;
    let total = min(atomicLoad(&resid_counters[3]), compact.params.y);
    var kind = 3u;
    if index < total {
        kind = residual_kind(resid_aux[index]);
        atomicAdd(&scatter_counts[kind], 1u);
    }
    workgroupBarrier();
    if lane < 3u {
        let count = atomicLoad(&scatter_counts[lane]);
        scatter_bases[lane] = atomicAdd(&resid_counters[8u + lane], count);
        atomicStore(&scatter_counts[lane], 0u);
    }
    workgroupBarrier();
    if kind < 3u {
        let local = atomicAdd(&scatter_counts[kind], 1u);
        let offset = atomicLoad(&resid_counters[5u + kind]);
        resid_work[offset + scatter_bases[kind] + local] = index;
    }
}

// The compact hot pass, or on an overflow the fused kernel, as an indirect
// dispatch over the block grid.
@compute @workgroup_size(1)
fn prepare_haze_output() {
    let overflow = atomicLoad(&resid_counters[4]) != 0u;
    let x = atomicLoad(&resid_args[18]);
    let y = atomicLoad(&resid_args[19]);
    atomicStore(&resid_args[12], select(x, 0u, overflow));
    atomicStore(&resid_args[13], y);
    atomicStore(&resid_args[14], 1u);
    atomicStore(&resid_args[15], select(0u, x, overflow));
    atomicStore(&resid_args[16], y);
    atomicStore(&resid_args[17], 1u);
}

// --- 3. residual -----------------------------------------------------------

fn residual_entry(gid: u32) {
    if gid >= atomicLoad(&resid_counters[RESID_KIND]) { return; }
    let index = resid_work[atomicLoad(&resid_counters[5u + RESID_KIND]) + gid];
    let entry = resid_list[index];
    let dense = (entry >> 22u) & 0x1FFu;
    if dense >= compact.params.x { return; }
    let li = compact_light(dense);
    // Retained list entries can outlive this frame's active light.
    if li == 0xFFFFFFFFu { return; }
    let pixel_index = entry & 0x3FFFFFu;
    let walked = entry >> 31u;
    let width = u32(haze.transport.w);
    let pixel = vec2<u32>(pixel_index % width, pixel_index / width);
    cache_lane = (pixel.y & 3u) * 8u + (pixel.x & 7u);
    cache_bx = pixel.x / HAZE_GROUP_X;
    cache_by = pixel.y / HAZE_GROUP_Y;
    cache_block = cache_by * cache_params.params.x + cache_bx;
    let ray = scene_ray(vec2<f32>(pixel) + 0.5);
    let span = beam_span(li, ray);
    var result = vec3<f32>(0.0);
    if RESID_KIND == 0u {
        if walked == 1u {
            var sum = vec3<f32>(haze.tiles.w);
            sum += lit_interval(li, ray, span.x, span.y);
            result = sum;
        } else {
            result = lit_interval(li, ray, span.x, span.y);
        }
    } else if RESID_KIND == 1u {
        let descriptor = resid_aux[index];
        let lookup = cache_slot(li);
        if lookup.mode != 0u
            && cache_entry_matches(descriptor, cache_block, lookup.key, lookup.gen) {
            result = cache_replay(li, ray, descriptor);
        } else {
            // An inactive slot makes its hash-table way reclaimable. A
            // retained record must traverse exactly if that entry was claimed
            // by another light while it was off.
            result = beam_shadow_integral(li, ray, span);
        }
    } else {
        result = beam_shadow_integral(li, ray, span);
    }
    resid_rgb[index * 3u] = result.x;
    resid_rgb[index * 3u + 1u] = result.y;
    resid_rgb[index * 3u + 2u] = result.z;
}

@compute @workgroup_size(RESID_LANES)
fn compute_residual(@builtin(workgroup_id) group: vec3<u32>,
                    @builtin(local_invocation_index) lane: u32) {
    let flat_group = group.y * compact.params.w + group.x;
    residual_entry(flat_group * RESID_LANES + lane);
}

// --- 4. hot ----------------------------------------------------------------

// `beam_scatter_cached` up to its ballot; the ballot's answer is read back
// from the classify pass.
fn beam_scatter_hot(li: u32, ray: SceneRay) -> vec3<f32> {
    if light_rest[li].haze_gain <= 0.0 { return vec3<f32>(0.0); }
    if light_rest[li].shadow_slot < 0.0 { return beam_scatter_exact(li, ray, lane_span(li, ray)); }
    let lookup = cache_slot(li);
    let plane = (cache_block * compact.params.x + compact_dense(lookup.slot)) * 2u;
    let r = resid_planes[plane];
    if r == 0u {
        // Uniform whole-lit, or no lane sees the light at all.
        if resid_planes[plane + 1u] == RESID_EMPTY { return vec3<f32>(0.0); }
        let span = lane_span(li, ray);
        if !block_one_subgroup {
            // Classified by a one-subgroup block; this pipeline's is not. Every
            // lane is whole-lit, which the traversal integrates bit for bit.
            return beam_scatter_exact(li, ray, span);
        }
        if !subgroupAny(span.y > span.x) { return vec3<f32>(0.0); }
        return quad_scatter(li, ray, span);
    }
    if ((r >> cache_lane) & 1u) == 0u { return vec3<f32>(0.0); }
    let index = resid_planes[plane + 1u] + countOneBits(r & ((1u << cache_lane) - 1u));
    if index >= compact.params.y { return vec3<f32>(0.0); }
    return vec3<f32>(resid_rgb[index * 3u], resid_rgb[index * 3u + 1u], resid_rgb[index * 3u + 2u]);
}

@compute @workgroup_size(HAZE_BLOCK_LANES)
fn compute_haze_hot(@builtin(workgroup_id) group: vec3<u32>,
                    @builtin(local_invocation_index) local: u32,
                    @builtin(subgroup_invocation_id) sub_lane: u32,
                    @builtin(num_subgroups) subgroups: u32) {
    let pixel = block_locate(group.xy, local, sub_lane, subgroups);
    let frag = vec2<f32>(pixel) + 0.5;
    let ray = scene_ray(frag);
    if block_one_subgroup { quad_prepare(frag, ray); }
    var deterministic = vec3<f32>(0.0);
    if haze.params.y >= 0.001 {
        var cursor = native_lights(frag);
        var li = 0u;
        while light_index_next(&cursor, &li) {
            deterministic += beam_scatter_hot(li, ray);
        }
    }
    if !cache_in_bounds { return; }
    textureStore(haze_out, vec2<i32>(pixel), native_output(frag, ray, deterministic));
}
