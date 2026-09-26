// Probe debug view: each reflection probe as a small ball where it stands,
// showing its cube. A point on the ball shows what the probe sees along the
// ball's normal there, so the ball is the probe's view wrapped round it: the
// floor's colour underneath, the rig above, a red pool on the side it lies.
// Unlit and drawn into the scene pass, so it takes the frame's exposure.

const PROBE_DEBUG_RADIUS: f32 = 0.3;
const PROBE_DEBUG_RINGS: u32 = 12u;
const PROBE_DEBUG_SEGMENTS: u32 = 24u;

struct ProbeDebugOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) @interpolate(flat) probe: u32,
};

/// A UV sphere from the vertex index alone: two triangles per quad,
/// `PROBE_DEBUG_RINGS * PROBE_DEBUG_SEGMENTS * 6` vertices a ball.
fn probe_debug_normal(vertex: u32) -> vec3<f32> {
    let quad = vertex / 6u;
    let corner = vertex % 6u;
    // Corners of the quad's two triangles, as (segment, ring) offsets.
    var offsets = array<vec2<u32>, 6>(
        vec2<u32>(0u, 0u), vec2<u32>(1u, 0u), vec2<u32>(1u, 1u),
        vec2<u32>(0u, 0u), vec2<u32>(1u, 1u), vec2<u32>(0u, 1u),
    );
    let at = vec2<u32>(quad % PROBE_DEBUG_SEGMENTS, quad / PROBE_DEBUG_SEGMENTS) + offsets[corner];
    let phi = f32(at.x) / f32(PROBE_DEBUG_SEGMENTS) * 6.28318530718;
    let theta = f32(at.y) / f32(PROBE_DEBUG_RINGS) * 3.14159265359;
    return vec3<f32>(sin(theta) * cos(phi), sin(theta) * sin(phi), cos(theta));
}

@vertex
fn vs_probe_debug(
    @builtin(vertex_index) vertex: u32,
    @builtin(instance_index) probe: u32,
) -> ProbeDebugOut {
    let n = probe_debug_normal(vertex);
    let world = probe_grid.positions[probe].xyz + n * PROBE_DEBUG_RADIUS;
    var out: ProbeDebugOut;
    out.clip = globals.view_proj * vec4<f32>(world, 1.0);
    out.normal = n;
    out.probe = probe;
    return out;
}

@fragment
fn fs_probe_debug(in: ProbeDebugOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    // The cube holds the stage's change to the sky probe: add the sky back.
    var radiance = textureSampleLevel(
        probe_cubes,
        environment_sampler,
        probe_cube_direction(n),
        in.probe,
        probe_grid.step.w,
    ).rgb;
    if environment_params.enabled > 0.5 {
        radiance += textureSampleLevel(environment_specular, environment_sampler, environment_direction(n), 0.0).rgb
            * environment_params.intensity;
    }
    return vec4<f32>(max(radiance, vec3<f32>(0.0)), 1.0);
}
