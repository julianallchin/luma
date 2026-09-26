override PROFILE_SKIP_SURFACE_CLOUDS: bool = false;
// Apply outdoor transport before alpha blending. A nearby cable sees only
// the air in front of itself, not the kilometres of air behind it.
fn scene_radiance(color: vec3<f32>, delta: vec3<f32>, frag_xy: vec2<f32>) -> vec3<f32> {
    let aerial = aerial_radiance(color, delta);
    if PROFILE_SKIP_SURFACE_CLOUDS { return aerial; }
    let debug = u32(globals.params.w + 0.5);
    if globals.medium.max.w <= 0.0 || globals.medium.min.w <= 0.0
        || (debug >= 1u && debug <= 5u) { return aerial; }
    let distance = length(delta);
    let direction = delta / max(distance, 1e-6);
    let transmission = exp(-medium_optical_depth(globals.medium, globals.camera_pos.xyz, direction, distance));
    let depth = dot(delta, globals.camera_forward.xyz);
    return aerial * transmission
        + surface_haze_light(direction, frag_xy, depth, vec2<f32>(depth)) * (1.0 - transmission);
}

// The light the haze in front of a fragment scatters toward the camera: the
// sky's, and the sun's where the sun reaches the air past the stage and the
// clouds (`sun_shafts.wgsl`). Each fragment reads the shafts at its own view
// `depth`: a pixel is resolved from several fragments, and one depth for all
// of them took the far ground's shaft off the truss in front of it. `range`
// is the view depths the fragment covers (`upsample_shafts`).
fn surface_haze_light(direction: vec3<f32>, frag_xy: vec2<f32>, depth: f32, range: vec2<f32>) -> vec3<f32> {
    var lit = 1.0;
    // A single texel of full sun is bound when the pass did not run.
    if any(textureDimensions(sun_shaft_fraction) > vec2<u32>(1u)) {
        lit = upsample_shafts(sun_shaft_fraction, frag_xy * globals.viewport.zw, depth, range);
    }
    return outdoor_ambient_mean.rgb
        + outdoor_haze_sun(direction, aerial_sky.sun.xyz, globals.outdoor_sun) * lit;
}

// Camera transmittance for an opaque surface fragment. The fog prefix already
// integrated every column's extinction front to back for the haze pass; the
// grid modes reuse it, and only the mean-density tail past the grid's radial
// extent is still analytic. Mode 0 is the per-fragment march above.
fn surface_transmittance(direction: vec3<f32>, distance: f32, frag_xy: vec2<f32>) -> f32 {
    let m = globals.medium;
    let origin = globals.camera_pos.xyz;
    let mode = globals.surface_fog.y;
    if mode <= 0.0 || (mode >= 2.0 && distance < globals.surface_fog.z) {
        return exp(-medium_optical_depth(m, origin, direction, distance));
    }
    let span = medium_lighting_span(m, origin, direction, globals.surface_fog.x);
    // The first grid sample includes all extinction up to the lighting bounds.
    // It cannot represent a nearer surface, or a ray that misses those bounds
    // entirely (including an empty fixture list during blackout).
    if span.y <= span.x || distance <= span.x {
        return exp(-medium_optical_depth(m, origin, direction, distance));
    }
    let size = vec3<f32>(textureDimensions(surface_fog_grid));
    let radial = sqrt(clamp((min(distance, span.y) - span.x) / max(span.y - span.x, 1e-5), 0.0, 1.0));
    let z = (radial * (size.z - 1.0) + 0.5) / size.z;
    let uv = clamp(frag_xy * globals.viewport.zw, 0.5 / size.xy, 1.0 - 0.5 / size.xy);
    var transmission = textureSampleLevel(surface_fog_grid, haze_noise_sampler, vec3<f32>(uv, z), 0.0).a;
    if distance > span.y {
        // Past the grid: the global mean density, clipped at the ground.
        let tail = medium_span(m, origin, direction, distance);
        if tail.y > span.y {
            transmission *= exp(-medium_height_depth(m, origin, direction, vec2<f32>(span.y, tail.y)));
        }
    }
    return transmission;
}

// `scene_radiance` for the opaque scene pipeline, which knows its fragment
// position and may read the fog grid instead of marching.
fn surface_radiance(color: vec3<f32>, delta: vec3<f32>, frag_xy: vec2<f32>) -> vec3<f32> {
    let depth = dot(delta, globals.camera_forward.xyz);
    return surface_haze(aerial_radiance(color, delta), delta, frag_xy, vec2<f32>(depth));
}

// The haze half of `surface_radiance`, over light that has crossed the
// aerial perspective already. `range` is the view depths the fragment
// covers (`upsample_shafts`).
fn surface_haze(aerial: vec3<f32>, delta: vec3<f32>, frag_xy: vec2<f32>, range: vec2<f32>) -> vec3<f32> {
    if PROFILE_SKIP_SURFACE_CLOUDS { return aerial; }
    let debug = u32(globals.params.w + 0.5);
    if globals.medium.max.w <= 0.0 || globals.medium.min.w <= 0.0
        || (debug >= 1u && debug <= 5u) { return aerial; }
    let distance = length(delta);
    let direction = delta / max(distance, 1e-6);
    let transmission = surface_transmittance(direction, distance, frag_xy);
    let depth = dot(delta, globals.camera_forward.xyz);
    return aerial * transmission
        + surface_haze_light(direction, frag_xy, depth, range) * (1.0 - transmission);
}
