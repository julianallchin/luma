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
// permanent validation test asserts the two builders' masks are
// bit-identical. The mask is order-independent, so the parallel build has no
// ordering discipline to get right.

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
    // xyz apex, w range.
    apex_range: vec4<f32>,
    // xyz direction, w cos_field.
    dir_cos: vec4<f32>,
    // x z0, y z1 (view-depth span), zw unused.
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

// Bounding sphere of one tile's frustum wedge clipped to a depth span.
// Mirrored operation-for-operation from `light_index.rs::View::wedge_sphere`;
// the bit-identity gate depends on the two staying in lockstep.
fn wedge_sphere(tile_x: u32, tile_y: u32, z0: f32, z1: f32) -> vec4<f32> {
    let vw = f32(params.counts.y);
    let vh = f32(params.counts.z);
    let tan_half_fov = params.right.w;
    let aspect = params.up.w;
    let px0 = f32(tile_x * 8u);
    let py0 = f32(tile_y * 8u);
    let px1 = min(px0 + 8.0, vw);
    let py1 = min(py0 + 8.0, vh);
    var corners: array<vec3<f32>, 8>;
    var cursor = 0u;
    for (var iz = 0u; iz < 2u; iz = iz + 1u) {
        let z = select(z0, z1, iz == 1u);
        for (var iy = 0u; iy < 2u; iy = iy + 1u) {
            let py = select(py0, py1, iy == 1u);
            for (var ix = 0u; ix < 2u; ix = ix + 1u) {
                let px = select(px0, px1, ix == 1u);
                let sx = (2.0 * px / vw - 1.0) * tan_half_fov * aspect;
                let sy = (1.0 - 2.0 * py / vh) * tan_half_fov;
                corners[cursor] = params.eye.xyz
                    + params.right.xyz * (sx * z)
                    + params.up.xyz * (sy * z)
                    + params.forward.xyz * z;
                cursor = cursor + 1u;
            }
        }
    }
    var sum = vec3<f32>(0.0);
    for (var i = 0u; i < 8u; i = i + 1u) {
        sum = sum + corners[i];
    }
    let center = sum / 8.0;
    var radius_sq = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let d = corners[i] - center;
        radius_sq = max(radius_sq, dot(d, d));
    }
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
    let big_base = (wg.y * params.grid.z + wg.x) * MASK_WORDS;
    let out_base = (tile_y * params.grid.x + tile_x) * MASK_WORDS;
    for (var word = 0u; word < MASK_WORDS; word = word + 1u) {
        var candidates = big_masks[big_base + word];
        var bits = 0u;
        while candidates != 0u {
            let bit = firstTrailingBit(candidates);
            candidates &= candidates - 1u;
            let light = lights[word * 32u + bit];
            let r = light.rect;
            if r.x <= tile_x && tile_x <= r.z && r.y <= tile_y && tile_y <= r.w {
                if NARROW_PHASE {
                    let sphere = wedge_sphere(tile_x, tile_y, light.span.x, light.span.y);
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
            }
        }
        tile_masks[out_base + word] = bits;
    }
}
