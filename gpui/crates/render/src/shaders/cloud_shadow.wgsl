// Reading the cloud shadow map (`atmosphere_cloud_shadow.wgsl`). `params`
// is `SkyUniform::shadow`: xy the map's centre, km, z its side, km, zero
// when there is no map, w the layer's share of clear sky. A point
// in the air is carried down the sun to the ground the map is drawn on.
//
// Past the map's inner seven tenths the map is read mirrored back into
// them, so the ground and the air beyond it are shaded by more of the same
// layer. They used to take `w`, the layer's share of clear sky; but at a
// low sun a slant ray meets a cloud almost everywhere, the map is nearly
// all shadow, and the far ground and air in that share of sun drew the
// horizon brighter than the country in front of it. The mirror is seamless,
// and past the first twenty kilometres nothing shows it repeat.

/// `cloud_shadow_at` for a point in the air, which may be inside the layer
/// or above it: the map is the shadow at the layer's base, and it lifts
/// through the layer to nothing at its top. `layer_km` is the base and top.
fn cloud_shadow_in_air(
    map: texture_2d<f32>,
    samp: sampler,
    params: vec4<f32>,
    layer_km: vec2<f32>,
    sun: vec3<f32>,
    world_m: vec3<f32>,
) -> f32 {
    let shadow = cloud_shadow_at(map, samp, params, sun, world_m);
    let above = smoothstep(layer_km.x, max(layer_km.y, layer_km.x + 1e-3), world_m.z * 0.001);
    return mix(shadow, 1.0, above);
}

fn cloud_shadow_at(
    map: texture_2d<f32>,
    samp: sampler,
    params: vec4<f32>,
    sun: vec3<f32>,
    world_m: vec3<f32>,
) -> f32 {
    if params.z <= 0.0 || sun.z <= 0.0 {
        return 1.0;
    }
    let p = world_m * 0.001;
    let ground = p.xy - sun.xy / sun.z * max(p.z, 0.0);
    let uv = (ground - params.xy) / params.z + 0.5;
    // A triangle wave over the inner [0.15, 0.85]: the identity inside it.
    let t = (uv - 0.15) / 0.7;
    let mirrored = 0.15 + 0.7 * (1.0 - abs(fract(t * 0.5) * 2.0 - 1.0));
    return textureSampleLevel(map, samp, mirrored, 0.0).r;
}
