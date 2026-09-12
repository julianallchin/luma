// GPU build of the unified light index's tile masks.
//
// Two dispatches, tile-major, no atomics (`docs/design/light-index-unification.md` §7):
//
// 1. `big_tile_prepass` — one workgroup per 64 px big tile, 16 threads, each
//    owning one mask word (32 lights). Pure 2D rect overlap against the big
//    tile compacts the candidate set ~16× before the fine pass.
// 2. `tile_fill` — one workgroup per big tile, 64 threads, each owning one
//    8 px sub-tile. Each thread walks the big tile's candidate words, tests
//    its own tile against each candidate's rect, accumulates the word in a
//    register, and issues one plain store per word.
//
// The broad test is an exact integer rect overlap against rects computed once
// on the CPU; the narrow test (behind `NARROW_PHASE`, injected from
// `light_index.rs` at pipeline creation) is Wronski's cone/sphere test
// against the tile wedge clipped to the light's own depth span. Both are
// mirrored operation-for-operation in the CPU reference builder, and the
// permanent validation test asserts the full-range masks are bit-identical.
// The analytic-source mask also rejects spherical cones separated from the
// tile by a side plane; its GPU tests check actual cone support independently.
// The masks are order-independent, so the parallel build has no ordering
// discipline to get right.

const MASK_WORDS: u32 = 16u;
// 64 px big tile = 8 × 8 px tiles.
const BIG_FACTOR: u32 = 8u;

struct IndexParams {
    // columns, rows, big_columns, big_rows (8 px tile grid and 64 px big-tile grid)
    grid: vec4<u32>,
    // light_count, viewport width, viewport height, unused
    counts: vec4<u32>,
    // near, Z_BINS / (far - near), unused ×2
    depth: vec4<f32>,
    // Camera basis for the narrow phase: eye (w unused), right (w: tan half
    // fov), up (w: aspect), forward (w unused).
    eye: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    forward: vec4<f32>,
}

struct LightCull {
    // Inclusive 8 px tile rect: x0, y0, x1, y1.
    rect: vec4<u32>,
    // Source region for broad washes, full range for narrow/gobo lights.
    near_rect: vec4<u32>,
    // xyz apex, w range.
    apex_range: vec4<f32>,
    // xyz direction, w cos_field.
    dir_cos: vec4<f32>,
    // xy view-depth span; z analytic-source range, or zero for the full-range path.
    span: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: IndexParams;
@group(0) @binding(1) var<storage, read> lights: array<LightCull>;
@group(0) @binding(2) var<storage, read_write> big_masks: array<u32>;
@group(0) @binding(3) var<storage, read_write> tile_masks: array<u32>;

@compute @workgroup_size(16)
fn big_tile_prepass(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_index) word: u32,
) {
    let x0 = wg.x * BIG_FACTOR;
    let y0 = wg.y * BIG_FACTOR;
    let x1 = min(x0 + BIG_FACTOR - 1u, params.grid.x - 1u);
    let y1 = min(y0 + BIG_FACTOR - 1u, params.grid.y - 1u);
    var bits = 0u;
    for (var i = 0u; i < 32u; i = i + 1u) {
        let li = word * 32u + i;
        if li >= params.counts.x {
            break;
        }
        let r = lights[li].rect;
        if r.x <= x1 && r.z >= x0 && r.y <= y1 && r.w >= y0 {
            bits |= 1u << i;
        }
    }
    big_masks[(wg.y * params.grid.z + wg.x) * MASK_WORDS + word] = bits;
}

// The z-independent half of one tile's frustum wedge: the screen-space
// corner coordinates, scaled by tan(fov/2). Computed once per tile; the
// per-light `wedge_sphere` only scales them by the light's depth span.
struct TileWedge {
    // x: left/right columns, y: top/bottom rows.
    sx: vec2<f32>,
    sy: vec2<f32>,
}

fn tile_wedge(tile_x: u32, tile_y: u32) -> TileWedge {
    let vw = f32(params.counts.y);
    let vh = f32(params.counts.z);
    let tan_half_fov = params.right.w;
    let aspect = params.up.w;
    let px0 = f32(tile_x * 8u);
    let py0 = f32(tile_y * 8u);
    let px1 = min(px0 + 8.0, vw);
    let py1 = min(py0 + 8.0, vh);
    return TileWedge(
        vec2<f32>(
            (2.0 * px0 / vw - 1.0) * tan_half_fov * aspect,
            (2.0 * px1 / vw - 1.0) * tan_half_fov * aspect,
        ),
        vec2<f32>((1.0 - 2.0 * py0 / vh) * tan_half_fov, (1.0 - 2.0 * py1 / vh) * tan_half_fov),
    );
}

fn wedge_corner(sx: f32, sy: f32, z: f32) -> vec3<f32> {
    return params.eye.xyz
        + params.right.xyz * (sx * z)
        + params.up.xyz * (sy * z)
        + params.forward.xyz * z;
}

// Bounding sphere of one tile's frustum wedge clipped to a depth span.
// Mirrored operation-for-operation from `light_index.rs::View::wedge_sphere`;
// the bit-identity gate depends on the two staying in lockstep. The eight
// corners are named registers (no private array), summed and reduced in
// the reference's order: z outer, then y, then x.
fn wedge_sphere(wedge: TileWedge, z0: f32, z1: f32) -> vec4<f32> {
    let c0 = wedge_corner(wedge.sx.x, wedge.sy.x, z0);
    let c1 = wedge_corner(wedge.sx.y, wedge.sy.x, z0);
    let c2 = wedge_corner(wedge.sx.x, wedge.sy.y, z0);
    let c3 = wedge_corner(wedge.sx.y, wedge.sy.y, z0);
    let c4 = wedge_corner(wedge.sx.x, wedge.sy.x, z1);
    let c5 = wedge_corner(wedge.sx.y, wedge.sy.x, z1);
    let c6 = wedge_corner(wedge.sx.x, wedge.sy.y, z1);
    let c7 = wedge_corner(wedge.sx.y, wedge.sy.y, z1);
    let center = (((((((c0 + c1) + c2) + c3) + c4) + c5) + c6) + c7) / 8.0;
    let d0 = c0 - center;
    let d1 = c1 - center;
    let d2 = c2 - center;
    let d3 = c3 - center;
    let d4 = c4 - center;
    let d5 = c5 - center;
    let d6 = c6 - center;
    let d7 = c7 - center;
    var radius_sq = max(0.0, dot(d0, d0));
    radius_sq = max(radius_sq, dot(d1, d1));
    radius_sq = max(radius_sq, dot(d2, d2));
    radius_sq = max(radius_sq, dot(d3, d3));
    radius_sq = max(radius_sq, dot(d4, d4));
    radius_sq = max(radius_sq, dot(d5, d5));
    radius_sq = max(radius_sq, dot(d6, d6));
    radius_sq = max(radius_sq, dot(d7, d7));
    return vec4<f32>(center, sqrt(radius_sq));
}

// Wronski's solid-cone/sphere test, transcribed from
// `light_index.rs::cone_reaches_sphere`. Conservative by contract: wrongly
// keeping a light costs a longer list, wrongly dropping one is an unlit hole.
fn cone_reaches_sphere(
    apex: vec3<f32>,
    direction: vec3<f32>,
    range: f32,
    cos_field: f32,
    centre: vec3<f32>,
    radius: f32,
) -> bool {
    let to_centre = centre - apex;
    let axial = dot(to_centre, direction);
    if axial > radius + range || axial < -radius {
        return false;
    }
    let perpendicular = sqrt(max(dot(to_centre, to_centre) - axial * axial, 0.0));
    let sin_field = sqrt(max(1.0 - cos_field * cos_field, 0.0));
    return cos_field * perpendicular - axial * sin_field <= radius;
}

// Maximum signed distance of a spherical cone from an inward tile plane.
// A negative maximum proves no ray in that tile can reach the source region.
fn source_reaches_plane(light: LightCull, normal: vec3<f32>) -> bool {
    // View::new switches to Y-up here; the scene still projects with Z-up.
    // Keep the existing rectangle mask until those camera conventions agree.
    if abs(params.forward.z) > 0.99 { return true; }
    let normal_length = length(normal);
    let axial = dot(normal, light.dir_cos.xyz);
    let cosine = light.dir_cos.w;
    var support = normal_length;
    if axial < normal_length * cosine {
        support = axial * cosine
            + sqrt(max(dot(normal, normal) - axial * axial, 0.0))
                * sqrt(max(1.0 - cosine * cosine, 0.0));
    }
    let range = light.span.z;
    // Expand for camera/position cancellation and shader intersection roundoff.
    let margin = (0.001 * max(range, 1.0)
        + 0.000004 * (length(params.eye.xyz) + length(light.apex_range.xyz))) * normal_length;
    return dot(light.apex_range.xyz - params.eye.xyz, normal)
        + range * max(support, 0.0) >= -margin;
}

@compute @workgroup_size(8, 8)
fn tile_fill(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
) {
    let tile_x = wg.x * BIG_FACTOR + local.x;
    let tile_y = wg.y * BIG_FACTOR + local.y;
    if tile_x >= params.grid.x || tile_y >= params.grid.y {
        return;
    }
    let low = vec2<f32>(vec2<u32>(tile_x, tile_y) * 8u);
    let high = min(low + 8.0, vec2<f32>(params.counts.yz));
    let sx = (2.0 * vec2<f32>(low.x, high.x) / f32(params.counts.y) - 1.0)
        * params.right.w * params.up.w;
    let sy = (1.0 - 2.0 * vec2<f32>(low.y, high.y) / f32(params.counts.z)) * params.right.w;
    let left = params.right.xyz - params.forward.xyz * sx.x;
    let right = params.forward.xyz * sx.y - params.right.xyz;
    let top = params.forward.xyz * sy.x - params.up.xyz;
    let bottom = params.up.xyz - params.forward.xyz * sy.y;
    let wedge = tile_wedge(tile_x, tile_y);
    let big_base = (wg.y * params.grid.z + wg.x) * MASK_WORDS;
    let out_base = (tile_y * params.grid.x + tile_x) * MASK_WORDS;
    for (var word = 0u; word < MASK_WORDS; word = word + 1u) {
        var candidates = big_masks[big_base + word];
        var bits = 0u;
        var near_bits = 0u;
        while candidates != 0u {
            let bit = firstTrailingBit(candidates);
            candidates &= candidates - 1u;
            let light = lights[word * 32u + bit];
            let r = light.rect;
            if r.x <= tile_x && tile_x <= r.z && r.y <= tile_y && tile_y <= r.w {
                if NARROW_PHASE {
                    let sphere = wedge_sphere(wedge, light.span.x, light.span.y);
                    if !cone_reaches_sphere(
                        light.apex_range.xyz,
                        light.dir_cos.xyz,
                        light.apex_range.w,
                        light.dir_cos.w,
                        sphere.xyz,
                        sphere.w,
                    ) {
                        continue;
                    }
                }
                bits |= 1u << bit;
                let n = light.near_rect;
                if n.x <= tile_x && tile_x <= n.z && n.y <= tile_y && tile_y <= n.w {
                    if light.span.z <= 0.0 || (source_reaches_plane(light, left)
                        && source_reaches_plane(light, right) && source_reaches_plane(light, top)
                        && source_reaches_plane(light, bottom)) { near_bits |= 1u << bit; }
                }
            }
        }
        tile_masks[out_base + word] = bits;
        tile_masks[params.grid.x * params.grid.y * MASK_WORDS + out_base + word] = near_bits;
    }
}

// The third and fourth mask planes refine surface candidates to visible
// shading depths. A tile whose visible depth span exceeds 1.5× (a cable or a
// truss member over the ground otherwise admits every cone between them) is
// split at the geometric mean of its bounds into two depth buckets, each
// with its own wedge sphere and plane; the split depth is stored per tile
// for the consumer (`scene.wgsl`), which picks the plane by its own view
// depth and falls back to the full-range plane within the R16 margin of the
// split. Exactness: every production-shaded fragment's centre depth is within
// that margin of a covered sample of its own pixel, the sample lies in exactly
// one bucket, and the bucket's sphere spans the bucket's samples widened by
// the margin. `SURFACE_SPLIT == 0u` is the single sphere over [z0, z1] with
// the split left at the sentinel; the fourth plane is then never written.
@group(0) @binding(4) var surface_depth: texture_multisampled_2d<f32>;
@group(0) @binding(5) var<storage, read_write> surface_splits: array<f32>;
var<workgroup> surface_near: array<f32, 64>;
var<workgroup> surface_far: array<f32, 64>;
var<workgroup> surface_near_b: array<f32, 64>;
var<workgroup> surface_far_b: array<f32, 64>;
var<workgroup> surface_bits: array<u32, 64>;
var<workgroup> surface_bits_b: array<u32, 64>;
var<workgroup> surface_sphere: vec4<f32>;
var<workgroup> surface_sphere_b: vec4<f32>;

// Unsplit sentinel: no finite view depth is within its margin.
const SURFACE_UNSPLIT: f32 = 1e30;

// R16 bounds need a whole relative half-float ULP of expansion. The
// final shader still uses full precision; only this conservative hull widens.
fn surface_margin(z0: f32, z1: f32) -> f32 {
    return max(abs(z0), abs(z1)) * 0.001 + 0.001;
}

fn surface_cull(sphere: vec4<f32>, margin: f32, mask: u32, word: u32) -> u32 {
    var bits = 0u;
    var remaining = mask;
    while remaining != 0u {
        let bit = firstTrailingBit(remaining);
        remaining &= remaining - 1u;
        let light = lights[word * 32u + bit];
        if cone_reaches_sphere(light.apex_range.xyz, light.dir_cos.xyz,
            light.apex_range.w, light.dir_cos.w, sphere.xyz, sphere.w + margin) {
            bits |= 1u << bit;
        }
    }
    return bits;
}

@compute @workgroup_size(64)
fn surface_fill(@builtin(workgroup_id) tile: vec3<u32>,
                @builtin(local_invocation_index) lane: u32) {
    let pixel = tile.xy * 8u + vec2<u32>(lane % 8u, lane / 8u);
    let in_bounds = all(pixel < params.counts.yz);
    var lo = 1e20;
    var hi = 0.0;
    if in_bounds {
        for (var sample = 0u; sample < textureNumSamples(surface_depth); sample += 1u) {
            let depth = textureLoad(surface_depth, vec2<i32>(pixel), i32(sample)).r;
            if depth != 0.0 {
                lo = min(lo, depth);
                hi = max(hi, depth);
            }
        }
    }
    surface_near[lane] = lo;
    surface_far[lane] = hi;
    workgroupBarrier();
    for (var stride = 32u; stride > 0u; stride /= 2u) {
        if lane < stride {
            surface_near[lane] = min(surface_near[lane], surface_near[lane + stride]);
            surface_far[lane] = max(surface_far[lane], surface_far[lane + stride]);
        }
        workgroupBarrier();
    }
    let z0 = surface_near[0];
    let z1 = surface_far[0];
    let valid = z1 >= z0 && z0 >= params.depth.x && z1 <= 60000.0;
    // Every lane derives the same split from the same workgroup values.
    var split = SURFACE_UNSPLIT;
    if SURFACE_SPLIT >= 2u && valid && z1 > 1.5 * z0 {
        split = sqrt(z0 * z1);
    }
    // Bucket bounds: A holds the samples at or before the split, B the rest.
    // Unsplit, A is the whole span and B stays empty.
    var lo_a = z0;
    var hi_a = z1;
    var lo_b = 1e20;
    var hi_b = 0.0;
    if SURFACE_SPLIT >= 2u {
        lo_a = 1e20;
        hi_a = 0.0;
        if in_bounds {
            for (var sample = 0u; sample < textureNumSamples(surface_depth); sample += 1u) {
                let depth = textureLoad(surface_depth, vec2<i32>(pixel), i32(sample)).r;
                if depth != 0.0 {
                    if depth <= split {
                        lo_a = min(lo_a, depth);
                        hi_a = max(hi_a, depth);
                    } else {
                        lo_b = min(lo_b, depth);
                        hi_b = max(hi_b, depth);
                    }
                }
            }
        }
        // Everyone has read z0/z1 before the arrays are reused.
        workgroupBarrier();
        surface_near[lane] = lo_a;
        surface_far[lane] = hi_a;
        surface_near_b[lane] = lo_b;
        surface_far_b[lane] = hi_b;
        workgroupBarrier();
        for (var stride = 32u; stride > 0u; stride /= 2u) {
            if lane < stride {
                surface_near[lane] = min(surface_near[lane], surface_near[lane + stride]);
                surface_far[lane] = max(surface_far[lane], surface_far[lane + stride]);
                surface_near_b[lane] = min(surface_near_b[lane], surface_near_b[lane + stride]);
                surface_far_b[lane] = max(surface_far_b[lane], surface_far_b[lane + stride]);
            }
            workgroupBarrier();
        }
        lo_a = surface_near[0];
        hi_a = surface_far[0];
        lo_b = surface_near_b[0];
        hi_b = surface_far_b[0];
    }
    let margin_a = surface_margin(lo_a, hi_a);
    let margin_b = surface_margin(lo_b, hi_b);
    if lane == 0u {
        surface_sphere = vec4<f32>(0.0);
        if valid {
            surface_sphere = wedge_sphere(tile_wedge(tile.x, tile.y), max(params.depth.x, lo_a - margin_a), hi_a + margin_a);
        }
    }
    if lane == 1u {
        surface_sphere_b = vec4<f32>(0.0);
        if valid && hi_b >= lo_b {
            surface_sphere_b = wedge_sphere(tile_wedge(tile.x, tile.y), max(params.depth.x, lo_b - margin_b), hi_b + margin_b);
        }
    }
    workgroupBarrier();
    let sphere = surface_sphere;
    let sphere_b = surface_sphere_b;
    // Four lanes own disjoint eight-bit sections of one 32-light word.
    let word = lane / 4u;
    let shift = (lane % 4u) * 8u;
    let base = (tile.y * params.grid.x + tile.x) * MASK_WORDS;
    let mask = tile_masks[base + word] & (255u << shift);
    var bits = 0u;
    var bits_b = 0u;
    if z0 < params.depth.x || z1 > 60000.0 {
        // Near-plane extrapolation and half-float overflow keep the
        // original ray mask instead of drawing conclusions from that depth.
        bits = mask;
    } else if z1 >= z0 {
        bits = surface_cull(sphere, margin_a, mask, word);
        if SURFACE_SPLIT >= 2u && split < SURFACE_UNSPLIT {
            bits_b = surface_cull(sphere_b, margin_b, mask, word);
        }
    }
    surface_bits[lane] = bits;
    surface_bits_b[lane] = bits_b;
    workgroupBarrier();
    let plane = params.grid.x * params.grid.y * MASK_WORDS;
    if lane < MASK_WORDS {
        let start = lane * 4u;
        tile_masks[2u * plane + base + lane] =
            surface_bits[start] | surface_bits[start + 1u] | surface_bits[start + 2u] | surface_bits[start + 3u];
        if SURFACE_SPLIT >= 2u {
            tile_masks[3u * plane + base + lane] =
                surface_bits_b[start] | surface_bits_b[start + 1u] | surface_bits_b[start + 2u] | surface_bits_b[start + 3u];
        }
    }
    if SURFACE_SPLIT >= 2u && lane == 0u {
        surface_splits[tile.y * params.grid.x + tile.x] = split;
    }
}
