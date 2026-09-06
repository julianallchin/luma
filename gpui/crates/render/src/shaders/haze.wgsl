@group(2) @binding(0) var fog_grid: texture_3d<f32>;

// Narrow beams and gobos use analytic cone/ray intersection and MIS transport.
// Dense, shadowed live rigs share distant broad-wash lighting in a 3D grid;
// only their four-metre source regions remain in the per-ray candidate list.
// The complementary smooth source/far windows sum to one. Captures with
// several subframes retain the full per-ray path for converged spatial detail.

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

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> HazeOutput {
    let ray = scene_ray(frag.xy);
    let weight = haze.tuning.y;
    let density = haze.params.y;

    if density < 0.001 {
        return HazeOutput(vec4<f32>(0.0, 0.0, 0.0, ray.view_depth * weight), vec4<f32>(0.0));
    }

    // Mean extinction, derived CPU-side from density (Transport::EXTINCTION —
    // the composite attenuates the scene with the same value). The noise
    // modulates in-scatter only; transmittance uses the mean so it stays an
    // analytic exp(-sigma*t) with no flicker.
    let sigma = haze.depth.z;

    var scattered = vec3<f32>(0.0);
    var sampled = vec3<f32>(0.0);

    // Ambient medium fill — diffuse haze the beams cut through. Closed-form
    // transmittance; eight stratified noise taps keep the drifting smoke
    // structure visible instead of averaging it flat. The taps only resolve the
    // near field; beyond it the noise (centred on 1) integrates as its mean, so
    // in-scatter saturates toward the medium's asymptotic colour along the whole
    // camera ray. Paired with the composite's matching extinction, a far surface
    // and the sky converge to the same fog instead of meeting at a silhouette.
    {
        let amb_end = min(ray.hit_dist, 24.0);
        let amb_step = amb_end / 8.0;
        var amb = 0.0;
        for (var i = 0; i < 8; i = i + 1) {
            let t = (f32(i) + ray.jitter) * amb_step;
            amb += haze_noise(haze.camera_pos.xyz + ray.dir * t, haze.params.w) * exp(-sigma * t);
        }
        let tail = exp(-sigma * amb_end) - exp(-sigma * ray.hit_dist);
        scattered += vec3<f32>(0.014, 0.011, 0.009) * density * (amb * sigma * amb_step + tail);
    }

    // This pass renders at a fraction of output resolution; the light index
    // is defined in full-resolution pixels, so scale the fragment coordinate
    // rather than rebuilding the index per consumer resolution.
    var cursor = lights_along(frag.xy * haze.tiles.xy);
    // The second mask plane shares the same tiles and sorted light ids.
    // Rejections are computed once per tile instead of once per haze pixel.
    if GRID_FOG {
        cursor.base += light_index_params.grid.x * light_index_params.grid.y * LIGHT_INDEX_WORDS;
        cursor.bits = light_index_masks[cursor.base + cursor.word];
    }
    var li = 0u;
    let group_size = select(max(u32(haze.depth.w), 1u), 4u, GRID_FOG);
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

    if GRID_FOG {
        let size = vec3<f32>(textureDimensions(fog_grid));
        let uv = clamp(frag.xy / vec2<f32>(haze.transport.w, haze.transport.z), 0.5 / size.xy, 1.0 - 0.5 / size.xy);
        let radial = sqrt(min(ray.hit_dist / haze.shadow.w, 1.0));
        let z = (radial * (size.z - 1.0) + 0.5) / size.z;
        sampled += textureSampleLevel(fog_grid, haze_noise_sampler, vec3<f32>(uv, z), 0.0).rgb * 256.0;
    }

    // Alpha carries linear view depth in metres so temporal rejection and the
    // composite's bilateral upsample have a distance-independent threshold.
    // Both channels are pre-weighted for subframe accumulation (spec §6).
    return HazeOutput(vec4<f32>(scattered * weight, ray.view_depth * weight), vec4<f32>(sampled * weight, 0.0));
}
