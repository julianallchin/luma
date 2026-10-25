// The haze's glow (`haze_glow.rs`): at a point, how much light the haze
// around it has scattered toward it, every way, per square metre: the
// fluence `haze_glow_gather.wgsl` gathered at each cell, past what blocks
// it. Declares no resources; each pass that prepends it binds
// `haze_glow_volume`, `haze_glow_sampler` and the `haze_glow` uniform
// itself.
//
// A cell is wide, and read straight a trilinear grid shows its cells as
// creases and lozenges on a surface where the glow fades. A surface reads
// it as a cubic B-spline instead, smooth across cells, in eight filtered
// taps; the haze along a camera ray adds up many reads, which smooths the
// creases away, and takes one.

struct HazeGlow {
    // xyz: the grid's low corner; w: mean extinction σ (1/m).
    min: vec4<f32>,
    // xyz: the grid's high corner; w: one with a glow, zero without.
    max: vec4<f32>,
};

fn haze_glow_uvw(world: vec3<f32>) -> vec3<f32> {
    return (world - haze_glow.min.xyz) / (haze_glow.max.xyz - haze_glow.min.xyz);
}

// One trilinear read, for the haze along a camera ray.
fn haze_glow_fluence_coarse(world: vec3<f32>) -> vec3<f32> {
    if haze_glow.max.w <= 0.0 {
        return vec3<f32>(0.0);
    }
    return textureSampleLevel(haze_glow_volume, haze_glow_sampler, haze_glow_uvw(world), 0.0).rgb;
}

// The smooth read, for a surface.
fn haze_glow_fluence(world: vec3<f32>) -> vec3<f32> {
    if haze_glow.max.w <= 0.0 {
        return vec3<f32>(0.0);
    }
    let size = vec3<f32>(textureDimensions(haze_glow_volume, 0));
    let x = haze_glow_uvw(world) * size - 0.5;
    let base = floor(x);
    let f = x - base;
    // The B-spline's four weights, folded into two filtered taps per axis.
    let f2 = f * f;
    let f3 = f2 * f;
    let w0 = (1.0 - 3.0 * f + 3.0 * f2 - f3) / 6.0;
    let w1 = (4.0 - 6.0 * f2 + 3.0 * f3) / 6.0;
    let w3 = f3 / 6.0;
    let w2 = 1.0 - w0 - w1 - w3;
    let g0 = w0 + w1;
    let g1 = w2 + w3;
    let low = (base - 0.5 + w1 / g0) / size;
    let high = (base + 1.5 + w3 / g1) / size;
    var light = vec3<f32>(0.0);
    for (var i = 0u; i < 8u; i += 1u) {
        let pick = vec3<bool>((i & 1u) != 0u, (i & 2u) != 0u, (i & 4u) != 0u);
        let at = select(low, high, pick);
        let weight = select(g0, g1, pick);
        light += weight.x * weight.y * weight.z
            * textureSampleLevel(haze_glow_volume, haze_glow_sampler, at, 0.0).rgb;
    }
    return light;
}
