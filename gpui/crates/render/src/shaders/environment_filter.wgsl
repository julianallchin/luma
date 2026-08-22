const PI: f32 = 3.14159265359;
const SAMPLE_COUNT: u32 = 64u;

struct Params { size: u32, mode: u32, roughness: f32, source_size: f32 };
@group(0) @binding(0) var source: texture_cube<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(3) var<uniform> params: Params;

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

fn radical_inverse(bits_in: u32) -> f32 {
    var bits = reverseBits(bits_in);
    return f32(bits) * 2.3283064365386963e-10;
}

fn basis(n: vec3<f32>, local: vec3<f32>) -> vec3<f32> {
    let up = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.z) > 0.999);
    let tangent = normalize(cross(up, n));
    return tangent * local.x + cross(n, tangent) * local.y + n * local.z;
}

fn cosine_sample(xi: vec2<f32>) -> vec3<f32> {
    let r = sqrt(xi.x);
    let phi = 2.0 * PI * xi.y;
    return vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(1.0 - xi.x, 0.0)));
}

fn importance_ggx(xi: vec2<f32>, n: vec3<f32>, roughness: f32) -> vec3<f32> {
    let alpha = roughness * roughness;
    let a2 = alpha * alpha;
    let phi = 2.0 * PI * xi.x;
    let cos_theta = sqrt((1.0 - xi.y) / max(1.0 + (a2 - 1.0) * xi.y, 1e-5));
    let sin_theta = sqrt(max(1.0 - cos_theta * cos_theta, 0.0));
    return normalize(basis(n, vec3<f32>(cos(phi) * sin_theta, sin(phi) * sin_theta, cos_theta)));
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.size || id.y >= params.size || id.z >= 6u { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / f32(params.size);
    let n = face_direction(id.z, uv);
    var sum = vec3<f32>(0.0);
    var weight = 0.0;
    for (var i = 0u; i < SAMPLE_COUNT; i++) {
        let xi = vec2<f32>((f32(i) + 0.5) / f32(SAMPLE_COUNT), radical_inverse(i));
        if params.mode == 0u {
            let l = basis(n, cosine_sample(xi));
            sum += textureSampleLevel(source, source_sampler, l, 0.0).rgb;
            weight += 1.0;
        } else {
            let h = importance_ggx(xi, n, max(params.roughness, 0.025));
            let l = normalize(2.0 * dot(n, h) * h - n);
            let dot_nl = max(dot(n, l), 0.0);
            if dot_nl > 0.0 {
                sum += textureSampleLevel(source, source_sampler, l, 0.0).rgb * dot_nl;
                weight += dot_nl;
            }
        }
    }
    textureStore(output_tex, vec2<i32>(id.xy), i32(id.z), vec4<f32>(sum / max(weight, 1e-5), 1.0));
}
