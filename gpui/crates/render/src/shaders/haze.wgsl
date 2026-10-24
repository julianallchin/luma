@group(2) @binding(0) var fog_grid: texture_3d<f32>;

// The compute kernels integrate every ungoboed beam deterministically and
// share distant broad-wash lighting in a 3D grid; only the washes' source
// regions remain in their per-pixel candidate list, and the complementary
// smooth source/far windows sum to one. The fragment pass is the MIS
// estimator for gobos, multi-subframe captures and the exhaustive reference.

// Independent integer stream for light selection: the integration jitter must
// not also choose the light, or the combined estimator becomes biased.
fn light_sample_hash(input: u32) -> u32 {
    var x = input;
    x = (x ^ (x >> 16u)) * 0x7feb352du;
    x = (x ^ (x >> 15u)) * 0x846ca68bu;
    return x ^ (x >> 16u);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle; no vertex buffer.
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    return vec4<f32>(xy, 0.0, 1.0);
}

struct HazeOutput {
    @location(0) exact: vec4<f32>,
    @location(1) sampled: vec4<f32>,
};

fn haze_at(frag: vec4<f32>) -> HazeOutput {
    var ray = scene_ray(frag.xy);
    let weight = haze.tuning.y;
    let density = haze.params.y;

    if density < 0.001 {
        return HazeOutput(vec4<f32>(0.0, 0.0, 0.0, ray.view_depth * weight), vec4<f32>(0.0));
    }

    // The same local density governs scattering and optical depth on both
    // the camera path and the cached light path.
    let sigma = haze.depth.z;

    var scattered = vec3<f32>(0.0);
    var sampled = vec3<f32>(0.0);
    ray.medium = medium_ray(haze.medium, haze.camera_pos.xyz, ray.dir, ray.hit_dist);

    // This pass renders at a fraction of output resolution; the light index
    // is defined in full-resolution pixels, so scale the fragment coordinate
    // rather than rebuilding the index per consumer resolution.
    var cursor = lights_along(frag.xy * haze.tiles.xy);
    var li = 0u;
    let group_size = max(u32(haze.depth.w), 1u);
    let seed = light_sample_hash(u32(frag.x) + u32(frag.y) * 65537u
        + u32(haze.tuning.x + haze.tiles.w) * 747796405u);
    var grouped = 0u;
    var selected = 0u;
    var total_importance = 0.0;
    var selected_importance = 1.0;
    while light_index_next(&cursor, &li) {
        let rest = light_rest[li];
        if group_size > 1u && rest.wash >= FOG_BROAD_WASH && rest.gobo < 0.5 && rest.haze_gain > 0.0 {
            // Weighted reservoir in small groups. The inverse selection
            // probability preserves each emitter's expected radiance. Narrow
            // beams and gobos retain exhaustive integration.
            grouped += 1u;
            let u = f32(light_sample_hash(seed ^ (li * 2891336453u)) >> 8u) / 16777216.0;
            let importance = beam_importance(li, ray, sigma);
            total_importance += importance;
            if u * total_importance < importance {
                selected = li;
                selected_importance = importance;
            }
            if grouped == group_size {
                if total_importance > 0.0 {
                    sampled += beam_scatter(selected, ray, sigma) * (total_importance / selected_importance);
                }
                grouped = 0u;
                total_importance = 0.0;
            }
        } else {
            scattered += beam_scatter(li, ray, sigma);
        }
    }
    if total_importance > 0.0 {
        sampled += beam_scatter(selected, ray, sigma) * (total_importance / selected_importance);
    }

    // Alpha carries linear view depth in metres so temporal rejection and the
    // composite's bilateral upsample have a distance-independent threshold.
    // Each subframe is weighted by its sample count.
    return HazeOutput(vec4<f32>(scattered * weight, ray.view_depth * weight), vec4<f32>(sampled * weight, 0.0));
}

// The light list of the compute kernels: the second mask plane, which drops
// broad washes beyond their source region (the grid carries those).
fn native_lights(frag: vec2<f32>) -> LightCursor {
    var cursor = lights_along(frag * haze.tiles.xy);
    cursor.base += light_index_params.grid.x * light_index_params.grid.y * LIGHT_INDEX_WORDS;
    cursor.bits = light_index_masks[cursor.base + cursor.word];
    return cursor;
}

// The compute kernels' output for one pixel: the per-light sum plus, on the
// first grid pass, the far-field grid's share, with linear view depth in
// alpha.
fn native_output(frag: vec2<f32>, ray: SceneRay, lights: vec3<f32>) -> HazeOutput {
    if haze.params.y < 0.001 {
        return HazeOutput(vec4<f32>(0.0, 0.0, 0.0, ray.view_depth * haze.tuning.y), vec4<f32>(0.0));
    }
    var deterministic = lights;
    let include_shared = haze.tiles.z > 1.5;
    if include_shared {
        let size = vec3<f32>(textureDimensions(fog_grid));
        let uv = clamp(frag / vec2<f32>(haze.transport.w, haze.transport.z), 0.5 / size.xy, 1.0 - 0.5 / size.xy);
        let span = ray.fog_span;
        let radial = sqrt(clamp((ray.hit_dist - span.x) / max(span.y - span.x, 1e-5), 0.0, 1.0));
        let z = (radial * (size.z - 1.0) + 0.5) / size.z;
        deterministic += textureSampleLevel(fog_grid, haze_noise_sampler, vec3<f32>(uv, z), 0.0).rgb * 256.0;
    }
    return HazeOutput(vec4<f32>(deterministic, ray.view_depth * select(0.0, 1.0, include_shared)), vec4<f32>(0.0));
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> HazeOutput {
    return haze_at(frag);
}

@group(2) @binding(1) var haze_exact_out: texture_storage_2d<rgba16float, write>;
@group(2) @binding(2) var haze_sampled_out: texture_storage_2d<rgba16float, write>;

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
    if haze.params.y >= 0.001 && haze.tiles.z > 0.5 {
        var cursor = native_lights(frag);
        var li = 0u;
        while light_index_next(&cursor, &li) {
            deterministic += beam_scatter_cached(li, ray);
        }
    }
    if !cache_in_bounds { return; }
    let result = native_output(frag, ray, deterministic);
    textureStore(haze_exact_out, vec2<i32>(pixel), result.exact);
    textureStore(haze_sampled_out, vec2<i32>(pixel), result.sampled);
}
