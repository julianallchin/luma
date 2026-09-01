// The sky as an environment probe, so that a rig standing under it is lit by
// it.
//
// This writes the *raw* cube the existing environment preprocessing already
// knows how to convolve (`environment.rs`), which is why the ambient term costs
// no new shading code: the scene pass reads the same irradiance and specular
// cubes it reads for an authored HDR. The face convention is three.js Y-up,
// matching `environment_equirect.wgsl`, because those cubes are sampled through
// `environment_direction`.
//
// No sun disc. The disc's power is the frame's directional light, and a probe
// that also carried it would light every surface twice.

@group(0) @binding(0) var skyview_lut: texture_2d<f32>;
@group(0) @binding(1) var lut_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(3) var<uniform> cfg: SkyUniform;

fn face_direction(face: u32, uv: vec2<f32>) -> vec3<f32> {
    let p = uv * 2.0 - 1.0;
    switch face {
        case 0u: { return normalize(vec3<f32>(1.0, -p.y, -p.x)); }
        case 1u: { return normalize(vec3<f32>(-1.0, -p.y, p.x)); }
        case 2u: { return normalize(vec3<f32>(p.x, 1.0, p.y)); }
        case 3u: { return normalize(vec3<f32>(p.x, -1.0, -p.y)); }
        case 4u: { return normalize(vec3<f32>(p.x, -p.y, 1.0)); }
        default: { return normalize(vec3<f32>(-p.x, -p.y, -1.0)); }
    }
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= SKY_CUBE_SIZE || id.y >= SKY_CUBE_SIZE || id.z >= 6u {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / f32(SKY_CUBE_SIZE);
    let three = face_direction(id.z, uv);
    // Three.js Y-up back to the renderer's Z-up world, the inverse of the
    // mapping `environment_direction` applies on the way in.
    let dir = vec3<f32>(three.x, -three.z, three.y);
    let sun = normalize(cfg.sun.xyz);
    let radius = sky_view_radius(cfg);
    let coords = skyview_coords(sun, dir);
    let sky = textureSampleLevel(
        skyview_lut,
        lut_sampler,
        skyview_uv(radius, coords.y, coords.x),
        0.0,
    ).rgb;
    textureStore(output_tex, vec2<i32>(id.xy), i32(id.z), vec4<f32>(sky * sky_exposure(cfg), 1.0));
}
