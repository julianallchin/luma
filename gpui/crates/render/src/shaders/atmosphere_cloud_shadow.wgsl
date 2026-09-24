// The layer's shadow: transmittance of sunlight through the clouds to each
// point of the ground, a top-down map around the camera (the cloud shadow
// map of Unreal and HDRP). Surfaces read it for the sun on the stage and the
// ground, and the air reads it along each ray, which is what cuts light
// shafts through the gaps. `cloud_shadow.wgsl` is the reader.

@group(0) @binding(9) var output_tex: texture_storage_2d<rgba16float, write>;
struct ShadowMap {
    // xy: the map's centre, km. z: its side, km.
    area: vec4<f32>,
    // x: the first row this dispatch writes, y: the stride between rows.
    // The wind moves the layer a few metres a second, so a live frame
    // redraws one row in `SHADOW_BANDS` and a still one redraws them all.
    rows: vec4<u32>,
};
@group(0) @binding(10) var<uniform> map: ShadowMap;

const SHADOW_STEPS: u32 = 24u;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output_tex);
    let row = id.y * map.rows.y + map.rows.x;
    if id.x >= size.x || row >= size.y {
        return;
    }
    let uv = (vec2<f32>(f32(id.x), f32(row)) + 0.5) / vec2<f32>(size);
    let ground = vec3<f32>(map.area.xy + (uv - 0.5) * map.area.z, 0.0);
    let sun = normalize(cfg.sun.xyz);
    var transmittance = 1.0;
    if sun.z > 0.0 {
        let span = cloud_span(ground, sun);
        if span.x < span.y {
            let dt = (span.y - span.x) / f32(SHADOW_STEPS);
            var depth = 0.0;
            for (var i = 0u; i < SHADOW_STEPS; i = i + 1u) {
                depth += cloud_extinction(ground + sun * (span.x + (f32(i) + 0.5) * dt), false) * dt;
            }
            transmittance = exp(-depth);
        }
        // The cirrus sheet: a faint, soft dimming, never a hard shadow.
        let sheet = ground.xy + sun.xy / sun.z * layer.cirrus.x;
        transmittance *= exp(-cirrus_optical_depth(sheet) / max(sun.z, 0.1));
    }
    textureStore(output_tex, vec2<i32>(i32(id.x), i32(row)), vec4<f32>(transmittance, 0.0, 0.0, 1.0));
}
