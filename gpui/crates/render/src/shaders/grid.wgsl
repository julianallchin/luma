// The fading floor grid, ported verbatim from `stage-visualizer.tsx`'s
// `GRID_FRAGMENT`. World-XY analytic grid (three's world-XZ), `fwidth`
// antialiasing, distance fade. Transparent, no depth write.

const CELL_SIZE: f32 = 0.5;
const SECTION_SIZE: f32 = 3.0;
const FADE_DISTANCE: f32 = 50.0;
const FADE_STRENGTH: f32 = 2.0;
const OPACITY: f32 = 0.4;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @builtin(instance_index) instance: u32,
) -> VsOut {
    _ = normal;
    let world = instances[instance].model * vec4<f32>(position, 1.0);
    var out: VsOut;
    out.clip = globals.view_proj * world;
    out.world = world.xyz;
    return out;
}

fn grid_line(coord: vec2<f32>, size: f32, thickness: f32) -> f32 {
    let scaled = coord / size;
    let fw = fwidth(scaled);
    let grid = abs(fract(scaled - 0.5) - 0.5);
    let line = smoothstep(fw * (thickness + 0.5), fw * 0.5, grid);
    return max(line.x, line.y);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let coord = in.world.xy;
    let dist = length(in.world - globals.camera_pos.xyz);

    var fade = 1.0 - smoothstep(FADE_DISTANCE * 0.3, FADE_DISTANCE, dist);
    fade = pow(fade, FADE_STRENGTH);

    let minor = grid_line(coord, CELL_SIZE, 0.25);
    let major = grid_line(coord, SECTION_SIZE, 0.25);

    // Both cell and section colours are white today; the mix is kept so the
    // two can diverge without touching the call site.
    let color = vec3<f32>(1.0);
    let alpha = max(minor * 0.01, major * 0.04) * fade * OPACITY;
    return vec4<f32>(color, alpha);
}
