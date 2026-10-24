@group(2) @binding(0) var fog_grid: texture_3d<f32>;

// The compute kernels integrate every beam deterministically and share
// distant broad-wash lighting in a 3D grid; only the washes' source regions
// remain in their per-pixel candidate list, and the complementary smooth
// source/far windows sum to one. The fragment pass is the exhaustive
// reference's MIS estimator.

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle; no vertex buffer.
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    return vec4<f32>(xy, 0.0, 1.0);
}

// Alpha carries linear view depth in metres so the composite's bilateral
// upsample has a distance-independent threshold. Each reference subframe is
// weighted by its sample count.
@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    var ray = scene_ray(frag.xy);
    let weight = haze.tuning.y;
    if haze.params.y < 0.001 {
        return vec4<f32>(0.0, 0.0, 0.0, ray.view_depth * weight);
    }
    ray.medium = medium_ray(haze.medium, haze.camera_pos.xyz, ray.dir, ray.hit_dist);
    // This pass renders at a fraction of output resolution; the light index
    // is defined in full-resolution pixels, so scale the fragment coordinate
    // rather than rebuilding the index per consumer resolution.
    var cursor = lights_along(frag.xy * haze.tiles.xy);
    var li = 0u;
    var scattered = vec3<f32>(0.0);
    while light_index_next(&cursor, &li) {
        // The same local density governs scattering and optical depth on
        // both the camera path and the cached light path.
        scattered += beam_scatter(li, ray, haze.depth.z);
    }
    return vec4<f32>(scattered * weight, ray.view_depth * weight);
}

// The light list of the compute kernels: the second mask plane, which drops
// broad washes beyond their source region (the grid carries those).
fn native_lights(frag: vec2<f32>) -> LightCursor {
    var cursor = lights_along(frag * haze.tiles.xy);
    cursor.base += light_index_params.grid.x * light_index_params.grid.y * LIGHT_INDEX_WORDS;
    cursor.bits = light_index_masks[cursor.base + cursor.word];
    return cursor;
}

// The compute kernels' output for one pixel: the per-light sum plus the
// far-field grid's share, with linear view depth in alpha.
fn native_output(frag: vec2<f32>, ray: SceneRay, lights: vec3<f32>) -> vec4<f32> {
    if haze.params.y < 0.001 {
        return vec4<f32>(0.0, 0.0, 0.0, ray.view_depth);
    }
    let size = vec3<f32>(textureDimensions(fog_grid));
    let uv = clamp(frag / vec2<f32>(haze.transport.w, haze.transport.z), 0.5 / size.xy, 1.0 - 0.5 / size.xy);
    let span = ray.fog_span;
    let radial = sqrt(clamp((ray.hit_dist - span.x) / max(span.y - span.x, 1e-5), 0.0, 1.0));
    let z = (radial * (size.z - 1.0) + 0.5) / size.z;
    let shared_light = textureSampleLevel(fog_grid, haze_noise_sampler, vec3<f32>(uv, z), 0.0).rgb * 256.0;
    return vec4<f32>(lights + shared_light, ray.view_depth);
}

@group(2) @binding(1) var haze_out: texture_storage_2d<rgba16float, write>;

// The fused compute kernel: every light of every pixel in one dispatch, the
// lit-interval cache read and written in place. Lanes past the image edge
// run the light loop with empty spans, so every block exchange sees all 32.
@compute @workgroup_size(HAZE_BLOCK_LANES)
fn compute_haze(@builtin(workgroup_id) group: vec3<u32>,
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
            deterministic += beam_scatter_cached(li, ray);
        }
    }
    if !cache_in_bounds { return; }
    textureStore(haze_out, vec2<i32>(pixel), native_output(frag, ray, deterministic));
}
