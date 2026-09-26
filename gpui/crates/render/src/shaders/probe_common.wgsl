// Reflection probes (`probes.rs`), shared by their capture, relight and
// filter passes and by the scene pass that samples them.
//
// A grid of cube maps over the stage and the audience. Each cube's surfaces
// are captured once, as albedo, normal, roughness, metal and distance, and
// relit every frame by the frame's own sun, sky and fixtures, so a truss
// reflects the pool a fixture has just turned red in the same frame.
//
// The cubes hold a change to the sky probe, not radiance: the sky probe
// lights everything, and the probes add what the stage changes of it
// (`probe_relight.wgsl`). A texel that sees open sky, or open ground under
// a sky, holds nothing, so a point the stage does not touch is lit by the
// sky probe alone, inside the grid as past it.
//
// Cube space is the environment's: three's Y up, `(x, z, -y)` of the
// renderer's Z-up world (`environment_direction` in `scene.wgsl`, without its
// rotation). Faces run +X, -X, +Y, -Y, +Z, -Z, as WebGPU samples a cube.

struct ProbeGrid {
    // xyz: the first probe, world. w: 1 when the probes are live.
    origin: vec4<f32>,
    // xyz: spacing between probes, metres. w: the base mip the captures and
    // the relight write (0 on High, 1 on Low).
    step: vec4<f32>,
    // xyz: probes along each axis. w: mips from the base to one texel.
    dims: vec4<f32>,
    // The parallax box, world: where the captured surroundings are taken
    // to lie. w: metres past the grid's edge over which it fades out.
    box_min: vec4<f32>,
    // w: the relight's reach, metres: fixtures whose cone misses this ball
    // round a probe are not relit in it.
    box_max: vec4<f32>,
    // rgb: the ground's albedo. w: 0 with no ground plane; 1 with one the
    // probes light; 2 with one the sky probe holds (open air), of which the
    // probes keep only what the stage changes.
    ground: vec4<f32>,
    // Where each probe stands, world. A grid point inside geometry is moved
    // out of it, so this is not always the grid point (`probes.rs`).
    positions: array<vec4<f32>, PROBE_CAPACITY>,
};

// The distance a relit texel stores where the probe sees sky.
const PROBE_SKY_DISTANCE: f32 = 10000.0;

fn probe_cube_direction(world_direction: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(world_direction.x, world_direction.z, -world_direction.y);
}

fn probe_world_direction(cube_direction: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(cube_direction.x, -cube_direction.z, cube_direction.y);
}

/// Cube-space direction through `uv` (0..1, rows down) of `face`, not
/// normalised: the face's axis plus its in-plane offset.
fn probe_face_direction(face: u32, uv: vec2<f32>) -> vec3<f32> {
    let p = uv * 2.0 - 1.0;
    switch face {
        case 0u: { return vec3<f32>(1.0, -p.y, -p.x); }
        case 1u: { return vec3<f32>(-1.0, -p.y, p.x); }
        case 2u: { return vec3<f32>(p.x, 1.0, p.y); }
        case 3u: { return vec3<f32>(p.x, -1.0, -p.y); }
        case 4u: { return vec3<f32>(p.x, -p.y, 1.0); }
        default: { return vec3<f32>(-p.x, -p.y, -1.0); }
    }
}

fn probe_count(grid: ProbeGrid) -> u32 {
    return u32(grid.dims.x) * u32(grid.dims.y) * u32(grid.dims.z);
}

fn probe_cell(grid: ProbeGrid, index: u32) -> vec3<u32> {
    let nx = u32(grid.dims.x);
    let ny = u32(grid.dims.y);
    return vec3<u32>(index % nx, (index / nx) % ny, index / (nx * ny));
}

fn probe_index(grid: ProbeGrid, cell: vec3<u32>) -> u32 {
    return cell.x + u32(grid.dims.x) * (cell.y + u32(grid.dims.y) * cell.z);
}

/// Octahedral encoding of a unit vector into -1..1 squared.
fn probe_octahedral(n: vec3<f32>) -> vec2<f32> {
    let p = n.xy / (abs(n.x) + abs(n.y) + abs(n.z));
    if n.z >= 0.0 {
        return p;
    }
    return (1.0 - abs(p.yx)) * select(vec2<f32>(-1.0), vec2<f32>(1.0), p >= vec2<f32>(0.0));
}

fn probe_unoctahedral(e: vec2<f32>) -> vec3<f32> {
    var n = vec3<f32>(e.x, e.y, 1.0 - abs(e.x) - abs(e.y));
    let t = saturate(-n.z);
    n.x += select(t, -t, n.x >= 0.0);
    n.y += select(t, -t, n.y >= 0.0);
    return normalize(n);
}
