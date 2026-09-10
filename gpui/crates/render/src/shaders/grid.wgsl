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

// Downstage compass: an arrow on the floor pointing at the house (world −y),
// drawn analytically like the grid lines so it needs no mesh and fades with
// them. It exists because the facade's `v` axis and the tiles caption have
// both lied about this direction; the floor itself is now the authority.
// Painted by `fs_compass` below, on its own draw of the same quad, so
// `RenderSettings::show_gizmos` can withhold it without taking the grid.
// Sized and placed to survive both foreshortening from standing views and a
// stage parked over the origin: it starts 8 m out on the open house floor.
const ARROW_SHAFT_START_Y: f32 = -8.0;
const ARROW_SHAFT_END_Y: f32 = -13.5;
const ARROW_TIP_Y: f32 = -16.0;
const ARROW_SHAFT_HALF_WIDTH: f32 = 0.3;
const ARROW_HEAD_HALF_WIDTH: f32 = 1.5;
const ARROW_STROKE_HALF_WIDTH: f32 = 0.01;

fn arrow_mask(world_coord: vec2<f32>) -> f32 {
    // House = world +y (facade +v, through the data mirror — see coords.rs).
    // Mirror y for direction and fold x onto the outline's symmetric right half.
    let coord = vec2<f32>(abs(world_coord.x), -world_coord.y);
    // Intersect straight edge half-planes for sharp, mitered corners. The shaft
    // overlaps the head's base half-plane, leaving no internal seam to stroke.
    let shaft = min(ARROW_SHAFT_HALF_WIDTH - coord.x, ARROW_SHAFT_START_Y - coord.y);
    let base = ARROW_SHAFT_END_Y - coord.y;
    let slope = ARROW_HEAD_HALF_WIDTH / (ARROW_SHAFT_END_Y - ARROW_TIP_Y);
    let sides = (slope * (coord.y - ARROW_TIP_Y) - coord.x) / sqrt(1.0 + slope * slope);
    let distance = min(max(shaft, base), sides);
    let fw = max(fwidth(distance) * 0.5, 0.00001);
    // Preserve the coverage of a fine stroke even when it is narrower than a pixel.
    return smoothstep(-fw, fw, distance + ARROW_STROKE_HALF_WIDTH)
        - smoothstep(-fw, fw, distance - ARROW_STROKE_HALF_WIDTH);
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
    let color = scene_radiance(vec3<f32>(1.0), in.world - globals.camera_pos.xyz);
    let alpha = max(minor * 0.01, major * 0.04) * fade * OPACITY;
    let depth = dot(in.world - globals.camera_pos.xyz, globals.camera_forward.xyz);
    return vec4<f32>(color, alpha * horizon_coverage(depth));
}

@fragment
fn fs_compass(in: VsOut) -> @location(0) vec4<f32> {
    let dist = length(in.world - globals.camera_pos.xyz);
    // The compass is exempt from the grid's short fade: it must read from a
    // camera framing the whole rig, which stands well past FADE_DISTANCE.
    let fade = 1.0 - smoothstep(FADE_DISTANCE * 2.0, FADE_DISTANCE * 4.0, dist);
    let depth = dot(in.world - globals.camera_pos.xyz, globals.camera_forward.xyz);
    let color = scene_radiance(vec3<f32>(0.3), in.world - globals.camera_pos.xyz);
    return vec4<f32>(color, arrow_mask(in.world.xy) * fade * 0.25 * horizon_coverage(depth));
}
