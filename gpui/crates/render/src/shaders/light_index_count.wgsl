// Profiler-only fragment counting for the light index.
//
// One thread per full-resolution depth texel: linearise the reverse-Z depth,
// walk `lights_at` exactly as the surface pass would, and accumulate
// (geometry pixels, candidates walked). This runs as its own compute pass so
// the read-write counter buffer is never bound in the hot passes — bound
// there, Metal's hazard tracking serialised every pass that shared it,
// ~20× wall time per frame at high draw counts.
//
// The light-index consumer prelude (`light_index.wgsl`) is prepended and
// provides `lights_at` / `light_index_next` on group 1.

struct CountParams {
    // x: view near, y: view far, zw: unused.
    near_far: vec4<f32>,
}

@group(0) @binding(0) var<uniform> count_params: CountParams;
@group(0) @binding(1) var count_depth: texture_depth_2d;
@group(0) @binding(2) var<storage, read_write> count_out: array<atomic<u32>>;

@compute @workgroup_size(8, 8)
fn count_fragments(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(count_depth);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let raw = textureLoad(count_depth, vec2<i32>(gid.xy), 0);
    // Reverse-Z: cleared (far) depth is zero — sky pixels shade no lights.
    if raw <= 0.0 {
        return;
    }
    let near = count_params.near_far.x;
    let far = count_params.near_far.y;
    let view_depth = near * far / max(near + raw * (far - near), 1e-5);
    var cursor = lights_at(vec2<f32>(gid.xy) + vec2<f32>(0.5, 0.5), view_depth);
    var id = 0u;
    var walked = 0u;
    while light_index_next(&cursor, &id) {
        walked = walked + 1u;
    }
    atomicAdd(&count_out[0], 1u);
    atomicAdd(&count_out[1], walked);
}
