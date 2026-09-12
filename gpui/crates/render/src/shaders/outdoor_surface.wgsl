override PROFILE_SKIP_SURFACE_CLOUDS: bool = false;
// Apply outdoor transport before alpha blending. A nearby cable sees only
// the air in front of itself, not the kilometres of air behind it.
fn scene_radiance(color: vec3<f32>, delta: vec3<f32>) -> vec3<f32> {
    let aerial = aerial_radiance(color, delta);
    if PROFILE_SKIP_SURFACE_CLOUDS { return aerial; }
    let debug = u32(globals.params.w + 0.5);
    if globals.medium.max.w <= 0.0 || globals.medium.min.w <= 0.0
        || (debug >= 1u && debug <= 5u) { return aerial; }
    let distance = length(delta);
    let direction = delta / max(distance, 1e-6);
    let transmission = exp(-medium_optical_depth(globals.medium, globals.camera_pos.xyz, direction, distance));
    return aerial * transmission
        + outdoor_haze_light(direction, aerial_sky.sun.xyz, globals.outdoor_sun) * (1.0 - transmission);
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
    let aerial = aerial_radiance(color, delta);
    if PROFILE_SKIP_SURFACE_CLOUDS { return aerial; }
    let debug = u32(globals.params.w + 0.5);
    if globals.medium.max.w <= 0.0 || globals.medium.min.w <= 0.0
        || (debug >= 1u && debug <= 5u) { return aerial; }
    let distance = length(delta);
    let direction = delta / max(distance, 1e-6);
    let transmission = surface_transmittance(direction, distance, frag_xy);
    return aerial * transmission
        + outdoor_haze_light(direction, aerial_sky.sun.xyz, globals.outdoor_sun) * (1.0 - transmission);
}
