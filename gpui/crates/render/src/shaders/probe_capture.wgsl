// Probe capture: one cube face of one probe, as the surfaces it sees.
//
// Written once per layout (`probes.rs`), not per frame: albedo and roughness
// in the first target, the normal (octahedral), metal and the distance from
// the probe in the second. The relight reads them back every frame. A texel
// nothing covers keeps its cleared distance of zero, and the relight reads
// the sky there.
//
// The face is a 90-degree view down the cube face's axis. Depth is reverse-Z
// with no far plane: the clip `z` is a constant near distance, so the depth
// is `near / w` and the nearest surface keeps the greatest.

const PROBE_NEAR: f32 = 0.05;

struct ProbeFace {
    // xyz: the probe, world. w: the face, 0 to 5.
    probe: vec4<f32>,
    // rgb: the ground's albedo, the floor's mean colour (`floor::mean_color`).
    // The floor's own maps are tiled detail a 48-texel face cannot hold.
    ground: vec4<f32>,
};

@group(0) @binding(2) var<uniform> probe_face: ProbeFace;
@group(1) @binding(0) var capture_base_color: texture_2d<f32>;
@group(1) @binding(2) var capture_metallic_roughness: texture_2d<f32>;
@group(1) @binding(5) var capture_sampler: sampler;

struct CaptureVertex {
    @builtin(position) clip: vec4<f32>,
    // The point relative to the probe, world.
    @location(0) rel: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) @interpolate(flat) instance: u32,
    @location(3) uv: vec2<f32>,
};

struct CaptureTargets {
    // rgb: albedo, linear. a: perceptual roughness.
    @location(0) albedo: vec4<f32>,
    // xy: the world normal, octahedral. z: metal, or -1 on the ground plane.
    // w: distance from the probe, metres; zero where nothing was drawn.
    @location(1) normal: vec4<f32>,
};

@vertex
fn vs_capture(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
    @builtin(instance_index) instance: u32,
) -> CaptureVertex {
    _ = tangent;
    let inst = instances[instance];
    let world = inst.model * vec4<f32>(position, 1.0);
    let rel = world.xyz - probe_face.probe.xyz;
    let c = probe_cube_direction(rel);
    let face = u32(probe_face.probe.w + 0.5);
    // The face's axis and the offsets that reach its right and bottom edges:
    // a direction `axis + a * right + b * down` lands at uv `((a + 1) / 2,
    // (b + 1) / 2)`, which is where the relight looks it up.
    let axis = probe_face_direction(face, vec2<f32>(0.5));
    let right = probe_face_direction(face, vec2<f32>(1.0, 0.5)) - axis;
    let down = probe_face_direction(face, vec2<f32>(0.5, 1.0)) - axis;
    var out: CaptureVertex;
    out.clip = vec4<f32>(dot(c, right), -dot(c, down), PROBE_NEAR, dot(c, axis));
    out.rel = rel;
    out.normal = (inst.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    out.instance = instance;
    out.uv = uv;
    return out;
}

@fragment
fn fs_capture(in: CaptureVertex) -> CaptureTargets {
    let inst = instances[in.instance];
    // The side the probe sees. Not `front_facing`: the face projection
    // mirrors the picture, which swaps what the rasteriser calls the front.
    var n = normalize(in.normal);
    if dot(n, in.rel) > 0.0 {
        n = -n;
    }
    var albedo = probe_face.ground.rgb;
    var roughness = 0.9;
    // The ground's mark for the relight: under a sky it is the sky probe's.
    var metallic = -1.0;
    if inst.flags.x <= 0.5 {
        let base = textureSample(capture_base_color, capture_sampler, in.uv);
        let mr = textureSample(capture_metallic_roughness, capture_sampler, in.uv);
        albedo = inst.base_color.rgb * base.rgb;
        roughness = clamp(inst.emissive.a * mr.g, 0.05, 1.0);
        metallic = saturate(inst.base_color.a * mr.b);
    }
    var out: CaptureTargets;
    out.albedo = vec4<f32>(albedo, roughness);
    out.normal = vec4<f32>(probe_octahedral(n), metallic, max(length(in.rel), 1e-3));
    return out;
}
