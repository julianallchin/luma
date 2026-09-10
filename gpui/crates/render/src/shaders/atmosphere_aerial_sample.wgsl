// Scene group 0, shared by opaque surfaces, cables and floor decals. Applying
// transport before alpha blending preserves each layer's own path length.
// The byte layout is atmosphere::SkyUniform; the volume stores exposed light.
struct AerialSky {
    sun: vec4<f32>,
    params: vec4<f32>,
};
@group(0) @binding(6) var aerial_radiance_tex: texture_3d<f32>;
@group(0) @binding(7) var aerial_transmittance_tex: texture_3d<f32>;
@group(0) @binding(8) var aerial_sampler: sampler;
@group(0) @binding(9) var<uniform> aerial_sky: AerialSky;

fn aerial_radiance(color: vec3<f32>, delta: vec3<f32>) -> vec3<f32> {
    if aerial_sky.sun.w < 0.5 { return color; }
    let debug = u32(globals.params.w + 0.5);
    if debug >= 1u && debug <= 5u { return color; }
    let size = vec3<f32>(textureDimensions(aerial_radiance_tex));
    let coord = (aerial_coords(aerial_sky.sun.xyz, delta) * (size - 1.0) + 0.5) / size;
    let light = textureSampleLevel(aerial_radiance_tex, aerial_sampler, coord, 0.0).rgb;
    let transmittance = textureSampleLevel(aerial_transmittance_tex, aerial_sampler, coord, 0.0).rgb;
    return color * transmittance + light;
}
