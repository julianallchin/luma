// Apply outdoor transport before alpha blending. A nearby cable sees only
// the air in front of itself, not the kilometres of air behind it.
fn scene_radiance(color: vec3<f32>, delta: vec3<f32>) -> vec3<f32> {
    let aerial = aerial_radiance(color, delta);
    let debug = u32(globals.params.w + 0.5);
    if globals.medium.max.w <= 0.0 || globals.medium.min.w <= 0.0
        || (debug >= 1u && debug <= 5u) { return aerial; }
    let distance = length(delta);
    let direction = delta / max(distance, 1e-6);
    let transmission = exp(-medium_optical_depth(globals.medium, globals.camera_pos.xyz, direction, distance));
    return aerial * transmission
        + outdoor_haze_light(direction, aerial_sky.sun.xyz, globals.outdoor_sun) * (1.0 - transmission);
}
