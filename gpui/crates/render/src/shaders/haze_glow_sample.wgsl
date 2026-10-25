// The haze's glow (`haze_glow.rs`): at a point, how much light the haze
// around it has scattered toward it. Declares no resources; each pass that
// prepends it binds `haze_glow_volume`, `haze_glow_sampler` and the
// `haze_glow` uniform itself.
//
// Haze that scatters power `S` per cubic metre sends a point `r` away
// `S e^(−σr) / 4πr²` of it per square metre, every way: the fluence. Over a
// shell from r to 2r that comes to `S̄ r e^(−σr)`, S̄ the shell's mean, so
// each level of the grid's averages stands for the shell it is as wide as,
// and the sum of the levels is the fluence from the cell out to the room.

struct HazeGlow {
    // xyz: the grid's low corner; w: mean extinction σ (1/m).
    min: vec4<f32>,
    // xyz: the grid's high corner; w: levels, zero when there is no glow.
    max: vec4<f32>,
};

fn haze_glow_fluence(world: vec3<f32>) -> vec3<f32> {
    let levels = u32(haze_glow.max.w);
    if levels == 0u {
        return vec3<f32>(0.0);
    }
    let extent = haze_glow.max.xyz - haze_glow.min.xyz;
    let uvw = (world - haze_glow.min.xyz) / extent;
    let size = vec3<f32>(textureDimensions(haze_glow_volume, 0));
    let cell = extent / size;
    let width = (cell.x + cell.y + cell.z) / 3.0;
    let sigma = haze_glow.min.w;
    // The cell itself: a ball half a cell wide round the point.
    var fluence = textureSampleLevel(haze_glow_volume, haze_glow_sampler, uvw, 0.0).rgb * 0.5 * width;
    var r = 0.5 * width;
    for (var level = 1u; level < levels; level += 1u) {
        let shell = textureSampleLevel(haze_glow_volume, haze_glow_sampler, uvw, f32(level)).rgb;
        fluence += shell * r * exp(-sigma * 1.5 * r);
        r *= 2.0;
    }
    return fluence;
}
