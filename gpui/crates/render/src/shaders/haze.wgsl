// Volumetric haze — transliteration of `effects/volumetric-haze-pass.ts`.
//
// There is no global march. The unified light index (`light_index.wgsl`)
// restricts each pixel to cones whose finite volumes can reach it. Each
// candidate's contribution is the 1D integral of single-scatter along the
// exact span of the ray inside that light's
// cone∩range volume (analytic ray/convex-solid intersection), estimated with
// equiangular + uniform MIS sampling. Empty pixels cost a handful of
// intersection tests and zero march steps.
//
// The ray reconstruction and the per-light integral live in
// `beam_transport.wgsl`, which this file prepends — the same functions serve
// the per-beam proxy pass, so the two passes cannot draw two different beams.
// What remains here is the pass shape: one oversized triangle, the ambient
// medium bed, and the tile-list loop.

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle; no vertex buffer.
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    return vec4<f32>(xy, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let ray = scene_ray(frag.xy);
    let weight = haze.tuning.y;
    let density = haze.params.y;

    if density < 0.001 {
        return vec4<f32>(0.0, 0.0, 0.0, ray.view_depth * weight);
    }

    // Mean extinction, derived CPU-side from density (Transport::EXTINCTION —
    // the composite attenuates the scene with the same value). The noise
    // modulates in-scatter only; transmittance uses the mean so it stays an
    // analytic exp(-sigma*t) with no flicker.
    let sigma = haze.depth.z;

    var scattered = vec3<f32>(0.0);

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
    var li = 0u;
    while light_index_next(&cursor, &li) {
        scattered += beam_scatter(li, ray, sigma);
    }

    // Alpha carries linear view depth in metres so temporal rejection and the
    // composite's bilateral upsample have a distance-independent threshold.
    // Both channels are pre-weighted for subframe accumulation (spec §6).
    return vec4<f32>(scattered * weight, ray.view_depth * weight);
}
