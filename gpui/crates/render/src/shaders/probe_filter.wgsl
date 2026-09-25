// Probe prefilter: one mip of every probe from the mips above it.
//
// Filtered importance sampling (Křivánek and Colbert, GPU Gems 3, 20): eight
// GGX samples at this mip's roughness, each read from the finer mip whose
// texel matches the solid angle the sample stands for. The mips above are
// already filtered, so eight samples do the work of hundreds; the whole
// chain of a 48-texel probe grid costs less than its relight. Mip `m` of
// `M` holds roughness `m / (M - 1)`, as the sky probe's does.

const PROBE_FILTER_PI: f32 = 3.14159265359;
const PROBE_FILTER_SAMPLES: u32 = 8u;

struct ProbeFilterParams {
    // Output face size in texels.
    size: u32,
    // Cube-face layers to write: six per probe.
    layers: u32,
    roughness: f32,
    // The source view's finest face size, and its mip count.
    source_size: f32,
    source_mips: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var probe_source: texture_cube_array<f32>;
@group(0) @binding(1) var probe_source_sampler: sampler;
@group(0) @binding(2) var probe_target: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(3) var<uniform> filter_params: ProbeFilterParams;
// Each probe face's mean radiance: the lowest mip, six to a probe, for the
// scene pass's diffuse (`probe_sample.wgsl`).
@group(0) @binding(4) var<storage, read_write> probe_ambient_out: array<vec4<f32>>;
// y: the first layer this frame writes (`probe_relight.wgsl`).
@group(0) @binding(5) var<uniform> probe_filter_first: vec4<u32>;

fn probe_filter_basis(n: vec3<f32>, local: vec3<f32>) -> vec3<f32> {
    let up = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.z) > 0.999);
    let tangent = normalize(cross(up, n));
    return tangent * local.x + cross(n, tangent) * local.y + n * local.z;
}

/// GGX-filtered radiance round `n` of cube `cube` at `roughness`, from
/// `count` samples of the source view.
fn probe_filter_lobe(n: vec3<f32>, cube: i32, roughness: f32, count: u32) -> vec3<f32> {
    let alpha = roughness * roughness;
    let a2 = alpha * alpha;
    // Solid angle of one texel of the source's finest mip.
    let texel = 4.0 * PROBE_FILTER_PI / (6.0 * filter_params.source_size * filter_params.source_size);
    var sum = vec3<f32>(0.0);
    var weight = 0.0;
    for (var i = 0u; i < count; i++) {
        // A rotated Fibonacci set: even over the lobe with few samples.
        let xi = vec2<f32>(fract(f32(i) * 0.618034 + 0.31), (f32(i) + 0.5) / f32(count));
        let phi = 2.0 * PROBE_FILTER_PI * xi.x;
        let cos_theta = sqrt((1.0 - xi.y) / max(1.0 + (a2 - 1.0) * xi.y, 1e-5));
        let sin_theta = sqrt(max(1.0 - cos_theta * cos_theta, 0.0));
        let h = probe_filter_basis(n, vec3<f32>(cos(phi) * sin_theta, sin(phi) * sin_theta, cos_theta));
        let l = normalize(2.0 * dot(n, h) * h - n);
        let dot_nl = dot(n, l);
        if dot_nl <= 0.0 {
            continue;
        }
        // With n = v, the pdf of l is D / 4.
        let d = a2 / (PROBE_FILTER_PI * pow(cos_theta * cos_theta * (a2 - 1.0) + 1.0, 2.0));
        let sample_angle = 4.0 / (f32(count) * max(d, 1e-6));
        let lod = clamp(0.5 * log2(sample_angle / texel) + 1.0, 0.0, filter_params.source_mips - 1.0);
        sum += textureSampleLevel(probe_source, probe_source_sampler, l, cube, lod).rgb * dot_nl;
        weight += dot_nl;
    }
    return sum / max(weight, 1e-6);
}

@compute @workgroup_size(8, 8, 1)
fn probe_filter(@builtin(global_invocation_id) id: vec3<u32>) {
    let layer = id.z + probe_filter_first.y;
    if id.x >= filter_params.size || id.y >= filter_params.size || layer >= filter_params.layers {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / f32(filter_params.size);
    let n = normalize(probe_face_direction(layer % 6u, uv));
    let filtered = probe_filter_lobe(n, i32(layer / 6u), filter_params.roughness, PROBE_FILTER_SAMPLES);
    textureStore(probe_target, vec2<i32>(id.xy), i32(layer), vec4<f32>(filtered, 1.0));
}

// Low's three mips in one dispatch (`probe_filter_base`): mip 1 is
// `probe_target`, these are mips 2 and 3.
@group(0) @binding(6) var probe_target_2: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(7) var probe_target_3: texture_storage_2d_array<rgba16float, write>;

/// Low's whole chain from the base mip alone, one thread a texel of any
/// mip: no mip waits for the one above it. Its faces are 24 texels, so a
/// chain of dependent dispatches cost more in waiting than in work. The
/// rougher mips take more samples, as they cannot read a blurred mip. The
/// centre texel of the last mip, on the face's axis, is also the ambient
/// cube's value. `filter_params.size` is mip 1's size.
@compute @workgroup_size(64, 1, 1)
fn probe_filter_base(@builtin(global_invocation_id) id: vec3<u32>) {
    let layer = id.z + probe_filter_first.y;
    if layer >= filter_params.layers {
        return;
    }
    let s1 = filter_params.size;
    let s2 = max(s1 / 2u, 1u);
    let s3 = max(s2 / 2u, 1u);
    var i = id.x;
    var mip = 1u;
    var size = s1;
    if i >= s1 * s1 {
        i -= s1 * s1;
        mip = 2u;
        size = s2;
        if i >= s2 * s2 {
            i -= s2 * s2;
            mip = 3u;
            size = s3;
            if i >= s3 * s3 {
                return;
            }
        }
    }
    let xy = vec2<u32>(i % size, i / size);
    let uv = (vec2<f32>(xy) + 0.5) / f32(size);
    let n = normalize(probe_face_direction(layer % 6u, uv));
    let filtered = probe_filter_lobe(n, i32(layer / 6u), f32(mip) / 3.0, 16u << (mip - 1u));
    let value = vec4<f32>(filtered, 1.0);
    switch mip {
        case 1u: { textureStore(probe_target, vec2<i32>(xy), i32(layer), value); }
        case 2u: { textureStore(probe_target_2, vec2<i32>(xy), i32(layer), value); }
        default: {
            textureStore(probe_target_3, vec2<i32>(xy), i32(layer), value);
            if all(xy == vec2<u32>(size / 2u)) {
                probe_ambient_out[layer] = value;
            }
        }
    }
}

/// The ambient cube: each face's one-texel mip, read along its axis.
/// `filter_params.layers` faces, one thread each.
@compute @workgroup_size(64, 1, 1)
fn probe_ambient(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= filter_params.layers {
        return;
    }
    let axis = probe_face_direction(id.x % 6u, vec2<f32>(0.5));
    probe_ambient_out[id.x] = vec4<f32>(
        textureSampleLevel(probe_source, probe_source_sampler, axis, i32(id.x / 6u), filter_params.source_mips - 1.0).rgb,
        1.0,
    );
}
