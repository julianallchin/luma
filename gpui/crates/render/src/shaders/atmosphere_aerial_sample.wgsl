// Scene group 0, shared by opaque surfaces, cables and floor decals. Applying
// transport before alpha blending preserves each layer's own path length.
// The byte layout is atmosphere::SkyUniform; the volume stores exposed light.
struct AerialSky {
    sun: vec4<f32>,
    params: vec4<f32>,
    clouds: vec4<f32>,
    // The cloud shadow map's place (`cloud_shadow.wgsl`).
    shadow: vec4<f32>,
    camera: vec4<f32>,
    // rgb: the ground's albedo, the floor's mean colour.
    ground: vec4<f32>,
};
@group(0) @binding(6) var aerial_radiance_tex: texture_3d<f32>;
@group(0) @binding(7) var aerial_transmittance_tex: texture_3d<f32>;
@group(0) @binding(8) var aerial_sampler: sampler;
@group(0) @binding(9) var<uniform> aerial_sky: AerialSky;
@group(0) @binding(20) var aerial_cloud_shadow: texture_2d<f32>;

/// The cloud shadow map's own mean over the inner part of it that is read
/// (`cloud_shadow.wgsl`): an eight by eight grid of taps.
fn cloud_shadow_mean() -> f32 {
    var sum = 0.0;
    for (var j = 0u; j < 8u; j++) {
        for (var i = 0u; i < 8u; i++) {
            let uv = vec2<f32>(0.15) + (vec2<f32>(f32(i), f32(j)) + 0.5) / 8.0 * 0.7;
            sum += textureSampleLevel(aerial_cloud_shadow, aerial_sampler, uv, 0.0).r;
        }
    }
    return sum / 64.0;
}

/// How much of the sun a world point gets through the cloud layer, as the
/// pixel showing it sees it. `dx` and `dy` are the point's screen
/// derivatives: the pixel's footprint. The ground reads its own span
/// instead (`ground_cloud_shadow` in `scene.wgsl`); this is for the rest.
///
/// Toward the horizon one pixel of a surface spans hundreds of metres, then
/// kilometres, of depth. A single tap of the map there picks the shadow of
/// one cloud or the sun between two at random, and the far ground turns
/// into sunlit and shaded lines along the horizon. So the map is averaged
/// over the footprint's long axis, a tap per map texel up to sixteen, and a
/// footprint of kilometres takes the map's mean.
///
/// That mean is the map's own, not the layer's nominal share of sun. At a
/// low sun a slant ray meets a cloud almost everywhere, and a far ground that
/// took the nominal share drew a bright line along the horizon over a field
/// in shadow.
fn surface_cloud_shadow(world: vec3<f32>, dx: vec3<f32>, dy: vec3<f32>) -> f32 {
    let shadow = aerial_sky.shadow;
    if shadow.z <= 0.0 {
        return 1.0;
    }
    let axis = select(dy.xy, dx.xy, dot(dx.xy, dx.xy) > dot(dy.xy, dy.xy));
    let footprint = length(axis);
    let texel = shadow.z * 1000.0 / f32(textureDimensions(aerial_cloud_shadow).x);
    let taps = u32(clamp(ceil(footprint / texel), 1.0, 16.0));
    var sum = 0.0;
    for (var i = 0u; i < taps; i++) {
        let at = world + vec3<f32>(axis * ((f32(i) + 0.5) / f32(taps) - 0.5), 0.0);
        sum += cloud_shadow_at(aerial_cloud_shadow, aerial_sampler, shadow, aerial_sky.sun.xyz, at);
    }
    let local = sum / f32(taps);
    let far = smoothstep(1000.0, 4000.0, footprint);
    if far <= 0.0 {
        return local;
    }
    return mix(local, cloud_shadow_mean(), far);
}

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
