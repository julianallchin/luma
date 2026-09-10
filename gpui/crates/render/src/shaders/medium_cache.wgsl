// One prefix column for each angular cell of each light. Built once per frame,
// then reused by every pixel and subframe. Geometry shadows remain separate.
struct CacheUniform { medium: ProceduralMedium, count: vec4<u32>, };
struct CacheCore { position: vec3<f32>, range: f32, };
struct CacheRest {
    direction: vec3<f32>, cos_beam: f32,
    color: vec3<f32>, intensity: f32,
    cos_field: f32, wash: f32, gobo: f32, rotation: f32,
    shadow: f32, haze_gain: f32, inverse_right_length: f32, field_tangent: f32,
};
@group(0) @binding(0) var<uniform> cache: CacheUniform;
@group(0) @binding(1) var<storage, read> cores: array<CacheCore>;
@group(0) @binding(2) var<storage, read> rests: array<CacheRest>;
@group(0) @binding(3) var haze_noise_field: texture_3d<f32>;
@group(0) @binding(4) var haze_noise_sampler: sampler;
@group(0) @binding(5) var optical_cache: texture_storage_3d<rgba16float, write>;
var<workgroup> optical_segments: array<f32, 32>;
@compute @workgroup_size(32)
fn build_cache(@builtin(workgroup_id) column: vec3<u32>, @builtin(local_invocation_index) z: u32) {
    let angles = u32(cache.medium.shape.w);
    let li = column.z;
    if li >= cache.count.x { return; }
    let core = cores[li];
    let rest = rests[li];
    let direction = rest.direction;
    let helper = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), abs(direction.z) > 0.98);
    let right = cross(direction, helper) * rest.inverse_right_length;
    let up = cross(right, direction);
    let tangent = rest.field_tangent;
    let uv = vec2<f32>(column.xy) / f32(angles - 1u) * 2.0 - 1.0;
    let ray = normalize(direction + tangent * (uv.x * right + uv.y * up));
    let step = core.range / 32.0;
    var sum = 0.0;
    for (var tap = 0u; tap < 2u; tap += 1u) {
        let d = (f32(z) + (f32(tap) + 0.5) * 0.5) * step;
        sum += medium_density(cache.medium, core.position + ray * d);
    }
    optical_segments[z] = sum * 0.5 * step * cache.medium.min.w;
    workgroupBarrier();
    for (var stride = 1u; stride < 32u; stride *= 2u) {
        var prior = 0.0;
        if z >= stride { prior = optical_segments[z - stride]; }
        workgroupBarrier();
        optical_segments[z] += prior;
        workgroupBarrier();
    }
    let xy = vec2<i32>(vec2<u32>(li % 16u, li / 16u) * angles + column.xy);
    if z == 0u { textureStore(optical_cache, vec3<i32>(xy, 0), vec4<f32>(0.0)); }
    textureStore(optical_cache, vec3<i32>(xy, i32(z + 1u)), vec4<f32>(optical_segments[z]));
}
