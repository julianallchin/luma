const PI: f32 = 3.14159265359;
const SAMPLE_COUNT: u32 = 128u;
struct Params { size: vec4<u32> };
@group(0) @binding(0) var output_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(1) var<uniform> params: Params;

fn radical_inverse(bits_in: u32) -> f32 {
    return f32(reverseBits(bits_in)) * 2.3283064365386963e-10;
}
fn importance_ggx(xi: vec2<f32>, roughness: f32) -> vec3<f32> {
    let alpha = roughness * roughness;
    let a2 = alpha * alpha;
    let phi = 2.0 * PI * xi.x;
    let cos_theta = sqrt((1.0 - xi.y) / max(1.0 + (a2 - 1.0) * xi.y, 1e-5));
    let sin_theta = sqrt(max(1.0 - cos_theta * cos_theta, 0.0));
    return vec3<f32>(cos(phi) * sin_theta, sin(phi) * sin_theta, cos_theta);
}
fn geometry_schlick(dot_nv: f32, roughness: f32) -> f32 {
    let k = roughness * roughness * 0.5;
    return dot_nv / max(dot_nv * (1.0 - k) + k, 1e-5);
}
@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.size.x || id.y >= params.size.x { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / f32(params.size.x);
    let dot_nv = max(uv.x, 1e-4);
    let roughness = uv.y;
    let v = vec3<f32>(sqrt(max(1.0 - dot_nv * dot_nv, 0.0)), 0.0, dot_nv);
    var a = 0.0;
    var b = 0.0;
    for (var i = 0u; i < SAMPLE_COUNT; i++) {
        let xi = vec2<f32>((f32(i) + 0.5) / f32(SAMPLE_COUNT), radical_inverse(i));
        let h = importance_ggx(xi, roughness);
        let l = normalize(2.0 * dot(v, h) * h - v);
        let dot_nl = max(l.z, 0.0);
        let dot_nh = max(h.z, 0.0);
        let dot_vh = max(dot(v, h), 0.0);
        if dot_nl > 0.0 {
            let g = geometry_schlick(dot_nv, roughness) * geometry_schlick(dot_nl, roughness);
            let visibility = g * dot_vh / max(dot_nh * dot_nv, 1e-5);
            let fresnel = pow(1.0 - dot_vh, 5.0);
            a += (1.0 - fresnel) * visibility;
            b += fresnel * visibility;
        }
    }
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(a, b, 0.0, 1.0) / f32(SAMPLE_COUNT));
}
