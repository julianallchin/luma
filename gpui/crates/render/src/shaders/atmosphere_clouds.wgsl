// The cloud layer by direction from the venue, for the ambient probe
// (`atmosphere_cube.wgsl`): the sky a rig is lit by has the clouds in it.
// The picture itself is traced per frame from the real camera
// (`atmosphere_cloud_view.wgsl`); the probe is low frequency by nature and
// is marched only when the sun or the weather changes.

@group(0) @binding(9) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output_tex);
    if any(id.xy >= size) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(size);
    let dir = cloud_panorama_direction(uv);
    let origin = vec3<f32>(0.0, 0.0, cfg.params.z);
    let clouds = cloud_march(origin, dir, 0.5, u32(layer.march.x));
    textureStore(output_tex, vec2<i32>(id.xy), clouds.light);
}
