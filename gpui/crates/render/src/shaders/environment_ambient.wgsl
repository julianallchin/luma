// One frame-constant value: the mean of the diffuse probe's six axis lobes.
//
// The surface and composite shaders used to take these six cube samples per
// fragment for a value that is identical for every fragment in the frame.
// The sample order, sampler, mip level and per-term division are the same as
// the fragment code they replace, so the result is bit-identical.
@group(0) @binding(0) var source: texture_cube<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<storage, read_write> ambient: vec4<f32>;

@compute @workgroup_size(1)
fn main() {
    let axes = array<vec3<f32>, 6>(
        vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0),
        vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
        vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, -1.0),
    );
    var total = vec3<f32>(0.0);
    for (var i = 0u; i < 6u; i += 1u) {
        total += textureSampleLevel(source, source_sampler, axes[i], 0.0).rgb / 6.0;
    }
    ambient = vec4<f32>(total, 0.0);
}
