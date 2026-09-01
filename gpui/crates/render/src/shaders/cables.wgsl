// Rigging cables: the black drops a flown piece hangs on (`cables.rs`).
//
// The geometry arrives as world-space vertical strips, two vertices per
// height, with the fade band already resolved into a per-vertex alpha. All
// this stage does is give the strip a width: a cable is a centimetre or two
// across, which is a *line*, so the quad is expanded on screen about the
// cable's own axis rather than authored at a fixed world width that would
// vanish edge-on and alias to nothing at the back of the room.
//
// `uv.x` is the alpha, `uv.y` the ±1 side of the strip.

// Half a cable's thickness, world metres.
const HALF_WIDTH_M: f32 = 0.008;

// Floor on the expanded half-width, in NDC. Below roughly a pixel a line stops
// getting thinner and starts getting *dimmer* through coverage alone, which
// reads as a cable that flickers as the camera moves.
const MIN_NDC_HALF: f32 = 0.0011;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) alpha: f32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @builtin(instance_index) instance: u32,
) -> VsOut {
    _ = normal;
    let world = (instances[instance].model * vec4<f32>(position, 1.0)).xyz;

    // Perpendicular to both the cable (world +z) and the eye ray, so the strip
    // always turns its face to the camera. Looking straight down the cable the
    // cross product vanishes and any perpendicular will do — the strip is a
    // point on screen either way.
    let to_camera = globals.camera_pos.xyz - world;
    let across = cross(vec3<f32>(0.0, 0.0, 1.0), to_camera);
    let reach = length(across);
    let side = select(vec3<f32>(1.0, 0.0, 0.0), across / max(reach, 1e-6), reach > 1e-4);

    // The projection is linear in homogeneous coordinates, so the clip-space
    // offset of a world displacement is that displacement projected as a
    // direction — no second full transform, and no divide.
    let centre = globals.view_proj * vec4<f32>(world, 1.0);
    let offset = (globals.view_proj * vec4<f32>(side * HALF_WIDTH_M, 0.0)).xy;
    let ndc_half = length(offset) / max(centre.w, 1e-4);
    let widen = max(1.0, MIN_NDC_HALF / max(ndc_half, 1e-8));

    var out: VsOut;
    out.clip = vec4<f32>(centre.xy + offset * uv.y * widen, centre.z, centre.w);
    out.alpha = uv.x;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, in.alpha);
}
