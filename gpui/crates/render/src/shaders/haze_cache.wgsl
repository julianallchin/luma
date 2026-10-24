// Lit-interval cache for the deterministic compute kernel
// (`docs/design/haze-lit-interval-cache.md`). Appended to `haze.wgsl` only
// when the device has subgroup ballots; otherwise `gpu.rs` appends a stub
// whose `beam_scatter_cached` is plain `beam_scatter`. Bound only by the
// compute pipelines (group 2, bindings 4-7).
struct IntervalCacheParams {
    // x: cache blocks per row, y: hash bucket count minus one, zw: unused.
    params: vec4<u32>,
    // y: frame epoch, stamped into claims so an entry written this frame is
    //    never stolen by another writer while its body is in flight.
    flags: vec4<u32>,
    // Per shadow slot: header base word, x0 | y0 << 16, w | h << 16 (block
    // rect), mode | generation << 8. Mode 0 traverses without touching the
    // pool, 1 reads, 2 traverses and writes.
    slots: array<vec4<u32>, 512>,
};
@group(2) @binding(4) var<uniform> cache_params: IntervalCacheParams;
// Header words, two per (block, slot): bit `lane` of the first is set when
// the traversal clipped the pixel's ray out of the shadow frustum and
// integrated the whole span directly, bit `lane` of the second when it walked
// the map, found everything lit and integrated the whole span through its
// running sum (the two round differently under contraction).
@group(2) @binding(5) var<storage, read_write> cache_header: array<u32>;
// Claim word per payload entry: block + 1, zero when empty, or a reserved
// marker while a body is being written. Atomic so two pairs landing on the
// same free way in the same frame cannot interleave.
@group(2) @binding(6) var<storage, read_write> cache_claims: array<atomic<u32>>;
// Payload entries: generation, count << 16 | slot << 5 | lane, then CACHE_K
// (a, b) interval pairs as raw f32 bits.
@group(2) @binding(7) var<storage, read_write> cache_table: array<u32>;

// Quadrature quad-sharing. For a 2×2 pixel quad (lanes l, l^1, l^8, l^9 of
// the 8×4 subgroup block) whose four header bits say whole-lit for this
// light, one quadrature runs on the quad-centre ray over the quad mean of the
// four spans, clamped at hit_min — the quad's nearest opaque hit — with Gauss
// node j of every piece evaluated by quad lane j and the four partial sums
// combined by two xor shuffles. Each pixel then integrates its own ray from
// hit_min to its own span end, only when its surface is farther than
// max(QUAD_GATE_M, QUAD_GATE_FRAC × hit_min) behind the quad's nearest hit,
// so truss silhouettes stay exact. A block shares only when every lane of
// the subgroup is active and whole-lit, so it never runs the shared loop and
// the per-lane loop back to back.
//
// Pairs whose ray passes within QUAD_APEX_PX pixels of the source on screen
// are not shared: the 1/d² apex is too sharp for a quad-centre ray.
const QUAD_APEX_PX: f32 = 32.0;
const QUAD_GATE_M: f32 = 0.25;
const QUAD_GATE_FRAC: f32 = 0.01;
var<private> quad_j: u32;
var<private> quad_hit_min: f32;
var<private> quad_apex: f32;
var<private> quad_ray: SceneRay;

fn quad_prepare(frag: vec2<f32>, ray: SceneRay) {
    quad_j = (cache_lane & 1u) | (((cache_lane >> 3u) & 1u) << 1u);
    // Every in-bounds lane is active here; a quad with an out-of-bounds
    // partner reads an undefined value, but its ballot below never lets it
    // share.
    let near = min(ray.hit_dist, subgroupShuffleXor(ray.hit_dist, 1u));
    quad_hit_min = min(near, subgroupShuffleXor(near, 8u));
    // Radians per pixel at this ray, squared and scaled by the apex radius.
    let angle = length(ray.dir - subgroupShuffleXor(ray.dir, 1u));
    quad_apex = QUAD_APEX_PX * QUAD_APEX_PX * angle * angle;
    quad_ray = ray;
    quad_ray.hit_dist = quad_hit_min;
    // The ray through the corner the four pixels share, built exactly as
    // `scene_ray` builds a pixel ray.
    let size = vec2<f32>(haze.transport.w, haze.transport.z);
    let centre = floor(frag * 0.5) * 2.0 + 1.0;
    let uv = centre / size;
    let ndc_xy = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let far_world = world_from_ndc(vec3<f32>(ndc_xy, 0.5));
    quad_ray.dir = normalize(far_world - haze.camera_pos.xyz);
    quad_ray.uv = uv;
    quad_ray.fog_span = medium_lighting_span(haze.medium, haze.camera_pos.xyz, quad_ray.dir, haze.shadow.w);
}

// Whether this pixel's ray passes far enough from light `li`'s source on
// screen to share its quad's quadrature.
fn quad_apex_clear(li: u32, ray: SceneRay) -> bool {
    let oc = haze.camera_pos.xyz - light_core[li].position;
    let b = dot(oc, ray.dir);
    let oo = dot(oc, oc);
    return oo - b * b >= quad_apex * oo;
}

// `lit_interval` restricted to Gauss node `j` of every piece, returning the
// unscaled sum: the same pieces, the same node positions, the same per-piece
// boundary transmittance interpolation (Scheme A2), one node per piece.
fn lit_interval_node(li: u32, ray: SceneRay, a: f32, b: f32, j: u32) -> f32 {
    if b <= a { return 0.0; }
    let core = light_core[li];
    let rest = light_rest[li];
    let oc = haze.camera_pos.xyz - core.position;
    let delta = -dot(oc, ray.dir);
    let h = sqrt(max(dot(oc, oc) - delta * delta, haze.tuning.z));
    let th0 = atan((a - delta) / h);
    let th1 = atan((b - delta) / h);
    let pieces = quadrature_pieces(a, b, th0, th1);
    var nodes = array<f32, 4>(-0.8611363116, -0.3399810436, 0.3399810436, 0.8611363116);
    var weights = array<f32, 4>(0.3478548451, 0.6521451549, 0.6521451549, 0.3478548451);
    let node = nodes[j];
    let weight = weights[j];
    var sum = 0.0;
    let width = (th1 - th0) / f32(pieces);
    // The per-piece boundary transmittance fetches are split across the
    // quad: lane j fetches boundary 3g + j of piece group g (three pieces,
    // four boundaries) and each piece's two edges arrive by shuffle, so a
    // call with up to three pieces costs one fetch per lane, not pieces + 1.
    let quad_base = cache_lane & 0x16u;
    var group = 0xFFFFFFFFu;
    var mine = 0.0;
    for (var piece = 0u; piece < pieces; piece += 1u) {
        let center = th0 + (f32(piece) + 0.5) * width;
        // `pieces`, `th0`, `width` and the ray are quad-uniform here, so the
        // four lanes walk the same groups and every shuffle source is active.
        let g = piece / 3u;
        if g != group {
            group = g;
            let k = 3u * g + quad_j;
            mine = 0.0;
            if k <= pieces {
                let t_k = delta + h * tan(th0 + f32(k) * width);
                mine = transport_transmittance(ray, li, t_k, haze.camera_pos.xyz + ray.dir * t_k);
            }
        }
        let local = piece - 3u * g;
        let left_lane = quad_base + ((local & 1u) | ((local >> 1u) << 3u));
        let right_lane = quad_base + (((local + 1u) & 1u) | (((local + 1u) >> 1u) << 3u));
        let left_field = subgroupShuffle(mine, left_lane);
        let right_field = subgroupShuffle(mine, right_lane);
        let tangent = tan(center + node * width * 0.5);
        let t = delta + h * tangent;
        let q = oc + ray.dir * t;
        let d2 = dot(q, q);
        let distance = sqrt(d2);
        let angular = angular_profile(lens_cos_angle(q, rest.direction, rest.lens_distance, distance), rest.cos_beam, rest.cos_field);
        let phase = henyey_greenstein(-dot(q, ray.dir) / max(distance, 1e-4), haze.transport.y);
        let source_weight = select(1.0, 1.0 - smoothstep(FOG_SOURCE_INNER, FOG_SOURCE_OUTER, distance), rest.wash >= FOG_BROAD_WASH);
        let world = haze.camera_pos.xyz + ray.dir * t;
        let field = mix(left_field, right_field, node * 0.5 + 0.5);
        let value = angular * phase * beam_range_falloff(distance, core.range) * source_weight
            * haze_density_at(world) * field
            / max(lens_apex_distance2(q, rest.direction, rest.lens_distance), haze.tuning.z);
        sum += value * h * (1.0 + tangent * tangent) * width * 0.5 * weight;
    }
    return sum;
}

// The scalar shared by one 2×2 quad: the quad mean of the four spans,
// clamped at the quad's nearest hit, integrated on the quad-centre ray.
fn quad_shared_scalar(li: u32, span: vec2<f32>) -> f32 {
    var s = span;
    s += subgroupShuffleXor(s, 1u);
    s += subgroupShuffleXor(s, 8u);
    var shared_span = s * 0.25;
    shared_span.y = min(shared_span.y, quad_hit_min);
    var sum = lit_interval_node(li, quad_ray, shared_span.x, shared_span.y, quad_j);
    sum += subgroupShuffleXor(sum, 1u);
    sum += subgroupShuffleXor(sum, 8u);
    return sum;
}

// Each pixel bilinearly blends its quad's shared sum with the neighbouring
// quads' (lanes ^2, ^16, ^18 — the same subgroup, so all of them shared this
// light) at 2-pixel spacing, weighting a neighbour by how close its nearest
// hit is to ours, so a smooth field stops being a 2×2 box and a silhouette
// never blends across the cut.
fn quad_interpolate_scalar(sum: f32) -> f32 {
    let x = cache_lane & 7u;
    let y = cache_lane >> 3u;
    let wx = select(0.0, 0.25, ((x >> 1u) & 1u) != (x & 1u));
    let wy = select(0.0, 0.25, ((y >> 1u) & 1u) != (y & 1u));
    let sh = subgroupShuffleXor(sum, 2u);
    let sv = subgroupShuffleXor(sum, 16u);
    let sd = subgroupShuffleXor(sum, 18u);
    let hh = subgroupShuffleXor(quad_hit_min, 2u);
    let hv = subgroupShuffleXor(quad_hit_min, 16u);
    let hd = subgroupShuffleXor(quad_hit_min, 18u);
    let tol = max(QUAD_GATE_M, quad_hit_min * QUAD_GATE_FRAC);
    let gh = select(0.0, 1.0, abs(hh - quad_hit_min) <= tol);
    let gv = select(0.0, 1.0, abs(hv - quad_hit_min) <= tol);
    let gd = select(0.0, 1.0, abs(hd - quad_hit_min) <= tol);
    let w_h = wx * (1.0 - wy) * gh;
    let w_v = (1.0 - wx) * wy * gv;
    let w_d = wx * wy * gd;
    let w_o = (1.0 - wx) * (1.0 - wy);
    return (w_o * sum + w_h * sh + w_v * sv + w_d * sd) / (w_o + w_h + w_v + w_d);
}

fn quad_scale_shared(li: u32, sum: f32) -> vec3<f32> {
    let rest = light_rest[li];
    let tint = mix(rest.color, vec3<f32>(1.0), haze.transport.x);
    return tint * (sum * rest.intensity * rest.haze_gain * haze.tuning.w * haze.depth.z);
}

// The shared quadrature plus this pixel's own remainder behind the quad's
// nearest hit.
fn quad_scatter(li: u32, ray: SceneRay, span: vec2<f32>) -> vec3<f32> {
    var result = quad_scale_shared(li, quad_interpolate_scalar(quad_shared_scalar(li, span)));
    if ray.hit_dist - quad_hit_min > max(QUAD_GATE_M, quad_hit_min * QUAD_GATE_FRAC) && span.y > quad_hit_min {
        result += lit_interval(li, ray, max(span.x, quad_hit_min), span.y);
    }
    return result;
}

fn cache_ballot(whole: bool) -> u32 { return subgroupBallot(whole).x; }
// The lowest active lane stores the block's header words.
fn cache_elect() -> bool { return cache_lane == firstTrailingBit(subgroupBallot(true).x); }

const CACHE_ENTRY_WORDS: u32 = 2u + 2u * CACHE_K;
const CACHE_WAYS: u32 = 4u;
const CACHE_NONE: u32 = 0xFFFFFFFFu;
// A claim word while its entry's body is still being written: nobody else
// may take it, nobody may read it.
const CACHE_RESERVED: u32 = 0xFFFFFFFFu;

fn cache_hash(block: u32, key: u32) -> u32 {
    var h = block * 0x9E3779B1u;
    h ^= key * 0x85EBCA77u;
    h ^= h >> 15u;
    h *= 0xC2B2AE3Du;
    h ^= h >> 13u;
    return h;
}

// An entry is live only under its slot's current generation; anything older
// is free to claim and never replayed.
fn cache_entry_live(entry: u32) -> bool {
    let base = entry * CACHE_ENTRY_WORDS;
    let slot = (cache_table[base + 1u] & 0x3FFFu) >> 5u;
    let mode = cache_params.slots[slot].w;
    return (mode & 0xFFu) != 0u && (mode >> 8u) == cache_table[base];
}

// A compact residual payload may retain this raw table index while its light
// is inactive. Another light can reclaim a dead way in the meantime, so a
// retained descriptor is replayable only while the published claim and body
// still identify its current block, slot lane and generation.
fn cache_entry_matches(entry: u32, block: u32, key: u32, gen: u32) -> bool {
    if (atomicLoad(&cache_claims[entry]) & 0xFFFFFu) != block + 1u { return false; }
    let base = entry * CACHE_ENTRY_WORDS;
    return cache_table[base] == gen && (cache_table[base + 1u] & 0xFFFFu) == key;
}

fn cache_find(block: u32, key: u32, gen: u32) -> u32 {
    let bucket = (cache_hash(block, key) & cache_params.params.y) * CACHE_WAYS;
    for (var way = 0u; way < CACHE_WAYS; way += 1u) {
        let entry = bucket + way;
        if cache_entry_matches(entry, block, key, gen) { return entry; }
    }
    return CACHE_NONE;
}

// Replays the recorded call sequence: the same `lit_interval` arguments in the
// same order the traversal used, starting from the same zero.
fn cache_replay(li: u32, ray: SceneRay, entry: u32) -> vec3<f32> {
    let base = entry * CACHE_ENTRY_WORDS;
    let count = cache_table[base + 1u] >> 16u;
    // Same shape as `beam_shadow_integral`: the two frustum tails summed
    // first, then each run added in turn. An empty tail returns zero before
    // any arithmetic, exactly as it did during the traversal.
    let t0 = vec2<f32>(bitcast<f32>(cache_table[base + 2u]), bitcast<f32>(cache_table[base + 3u]));
    let t1 = vec2<f32>(bitcast<f32>(cache_table[base + 4u]), bitcast<f32>(cache_table[base + 5u]));
    var sum = lit_interval(li, ray, t0.x, t0.y) + lit_interval(li, ray, t1.x, t1.y);
    for (var i = 2u; i < count; i += 1u) {
        let a = bitcast<f32>(cache_table[base + 2u + i * 2u]);
        let b = bitcast<f32>(cache_table[base + 3u + i * 2u]);
        sum += lit_interval(li, ray, a, b);
    }
    return sum;
}

// Stores this pair's recorded intervals, claiming a free or dead way of its
// bucket, and returns the entry (or `CACHE_NONE`). A full bucket loses the
// entry; that pair then traverses on every later frame, which is slower but
// exact.
fn cache_store(block: u32, key: u32, gen: u32) -> u32 {
    let bucket = (cache_hash(block, key) & cache_params.params.y) * CACHE_WAYS;
    let epoch = cache_params.flags.y & 0xFFFu;
    var chosen = CACHE_NONE;
    for (var way = 0u; way < CACHE_WAYS; way += 1u) {
        let entry = bucket + way;
        let claim = atomicLoad(&cache_claims[entry]);
        let base = entry * CACHE_ENTRY_WORDS;
        if (claim & 0xFFFFFu) == block + 1u && cache_table[base] == gen && (cache_table[base + 1u] & 0xFFFFu) == key {
            // A repeated pass of the same frame rewrites its own entry.
            chosen = entry;
            break;
        }
        // Free, or dead and not claimed this frame: an entry claimed this
        // frame may still have its body in flight, so it is never stolen.
        if claim == 0u || (claim != CACHE_RESERVED && (claim >> 20u) != epoch && !cache_entry_live(entry)) {
            // Reserve first, publish the claim last, so a second writer that
            // lands on this way sees it taken rather than a half-written body.
            if atomicCompareExchangeWeak(&cache_claims[entry], claim, CACHE_RESERVED).exchanged {
                chosen = entry;
                break;
            }
        }
    }
    if chosen == CACHE_NONE { return CACHE_NONE; }
    let base = chosen * CACHE_ENTRY_WORDS;
    cache_table[base] = gen;
    cache_table[base + 1u] = cache_count << 16u | key;
    for (var i = 0u; i < CACHE_K; i += 1u) {
        if i < cache_count {
            cache_table[base + 2u + i * 2u] = bitcast<u32>(cache_intervals[i].x);
            cache_table[base + 3u + i * 2u] = bitcast<u32>(cache_intervals[i].y);
        }
    }
    atomicStore(&cache_claims[chosen], epoch << 20u | (block + 1u));
    return chosen;
}

// `beam_scatter` for the deterministic compute kernel: the same prologue,
// the same traversal, with the cache branch around the traversal.
fn beam_scatter_cached(li: u32, ray: SceneRay, sigma: f32) -> vec3<f32> {
    let haze_gain = light_rest[li].haze_gain;
    if haze_gain <= 0.0 {
        return vec3<f32>(0.0);
    }
    let span = beam_span(li, ray);
    if span.y <= span.x || PROFILE_SKIP_NATIVE_INTEGRALS { return vec3<f32>(0.0); }
    if haze.shadow.x <= 0.0 || PROFILE_SKIP_NATIVE_SHADOWS { return lit_interval(li, ray, span.x, span.y); }
    var mode = 0u;
    var gen = 0u;
    var header = 0u;
    var key = 0u;
    let slot = u32(max(light_rest[li].shadow_slot, 0.0));
    let entry = cache_params.slots[slot];
    let x0 = entry.y & 0xFFFFu;
    let y0 = entry.y >> 16u;
    let w = entry.z & 0xFFFFu;
    let h = entry.z >> 16u;
    if (entry.w & 0xFFu) != 0u && cache_bx >= x0 && cache_bx - x0 < w && cache_by >= y0 && cache_by - y0 < h {
        mode = entry.w & 0xFFu;
        gen = entry.w >> 8u;
        header = entry.x + (cache_by - y0) * w + (cache_bx - x0);
        key = slot << 5u | cache_lane;
    }
    if mode == 1u {
        let whole = (((cache_header[header * 2u] | cache_header[header * 2u + 1u]) >> cache_lane) & 1u) == 1u
            && quad_apex_clear(li, ray);
        // Every lane still active here shares this light; a lane that left
        // early (empty span, out of bounds) reads as not whole-lit.
        if subgroupBallot(whole).x == 0xFFFFFFFFu {
            return quad_scatter(li, ray, span);
        }
        if ((cache_header[header * 2u] >> cache_lane) & 1u) == 1u {
            return lit_interval(li, ray, span.x, span.y);
        }
        if ((cache_header[header * 2u + 1u] >> cache_lane) & 1u) == 1u {
            // The traversal's `sum` started from two empty tails and took
            // one run: the same product added to a zero the compiler
            // cannot fold away (`haze.tiles.w` is always zero).
            var sum = vec3<f32>(haze.tiles.w);
            sum += lit_interval(li, ray, span.x, span.y);
            return sum;
        }
        let found = cache_find(cache_block, key, gen);
        if found != CACHE_NONE { return cache_replay(li, ray, found); }
    }
    let shadowed = beam_shadow_integral(li, ray, span);
    if mode == 2u {
        // One call over the full span: the ray was clipped out of the frustum.
        let direct = cache_count == 1u;
        // Two (empty) tails and one run equal to the full span.
        let walked = cache_count == 3u && all(cache_intervals[2] == span);
        // Every lane still active here shares this light and this block; the
        // ballots are the whole block's header words in one store each.
        let ballot_direct = cache_ballot(direct);
        let ballot_walked = cache_ballot(walked);
        if cache_elect() {
            cache_header[header * 2u] = ballot_direct;
            cache_header[header * 2u + 1u] = ballot_walked;
        }
        if !direct && !walked && cache_count <= CACHE_K { _ = cache_store(cache_block, key, gen); }
    }
    return shadowed;
}

fn cache_locate(pixel: vec2<u32>, lane: u32) {
    cache_lane = lane;
    cache_bx = pixel.x / HAZE_GROUP_X;
    cache_by = pixel.y / HAZE_GROUP_Y;
    cache_block = cache_by * cache_params.params.x + cache_bx;
}
