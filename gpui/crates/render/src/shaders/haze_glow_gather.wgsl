// The haze's glow gathered at each cell (`haze_glow.rs`): the light the haze
// round it has scattered toward it, past what stands in the way.
//
// Haze that scatters power `S` per cubic metre sends a point `r` away
// `S e^(−σr) / 4πr²` of it per square metre, every way: the fluence. Six
// cones, one along each way of each axis, each a sixth of the sphere, step
// out through shells r..2r. Over a shell a cone takes `S̄ r e^(−σr) / 6`,
// S̄ the shell's mean there, read from the level of the grid's averages as
// wide as the shell is deep, so each shell costs one tap however far out.
//
// What the cone has passed blocks it: the share of lines along its axis
// the geometry stops (`glow_occupancy.rs`), read a level finer, from the
// cells between this one and the shell and never this cell itself, so a
// surface does not shadow the light on its own open side.

struct Params {
    // xyz: the grid's low corner; w: mean extinction σ (1/m).
    min: vec4<f32>,
    // xyz: the grid's high corner; w: levels.
    max: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var cells: texture_3d<f32>;
@group(0) @binding(2) var cells_sampler: sampler;
@group(0) @binding(3) var blocking: texture_3d<f32>;
@group(0) @binding(4) var fluence: texture_storage_3d<rgba16float, write>;

@compute @workgroup_size(4, 4, 4)
fn gather(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(fluence);
    if any(id >= size) {
        return;
    }
    let extent = params.max.xyz - params.min.xyz;
    let cell = extent / vec3<f32>(size);
    let width = (cell.x + cell.y + cell.z) / 3.0;
    let sigma = params.min.w;
    let levels = u32(params.max.w);
    let here = (vec3<f32>(id) + 0.5) / vec3<f32>(size);
    // The cell itself: a ball half a cell wide round its middle, its haze
    // taken over the cells round it too, weighted 1 2 1 along each axis by
    // eight reads between cells, so a narrow beam's cells read as one
    // smooth line rather than a string of cell-shaped lumps.
    var own = vec3<f32>(0.0);
    for (var i = 0u; i < 8u; i += 1u) {
        let corner = vec3<f32>(f32(i & 1u), f32((i >> 1u) & 1u), f32((i >> 2u) & 1u)) - 0.5;
        own += textureSampleLevel(cells, cells_sampler, here + corner / vec3<f32>(size), 0.0).rgb;
    }
    var light = own * 0.125 * 0.5 * width;
    for (var cone = 0u; cone < 6u; cone += 1u) {
        let axis = cone / 2u;
        var way = vec3<f32>(0.0);
        way[axis] = select(1.0, -1.0, (cone & 1u) == 1u);
        var through = 1.0;
        var r = 0.5 * width;
        for (var level = 0u; level < levels; level += 1u) {
            let shell = here + way * 1.5 * r / extent;
            // Past the grid there is no haze to glow.
            if any(shell < vec3<f32>(0.0)) || any(shell > vec3<f32>(1.0)) {
                break;
            }
            // The cells between: the first shell's is the next cell, each
            // later one's half as wide as the shell's own.
            let between = select(shell, here + way * width / extent, level == 0u);
            let finer = f32(max(level, 1u) - 1u);
            through *= 1.0 - textureSampleLevel(blocking, cells_sampler, between, finer)[axis];
            let scattered = textureSampleLevel(cells, cells_sampler, shell, f32(level)).rgb;
            light += scattered * r * exp(-sigma * 1.5 * r) * through / 6.0;
            r *= 2.0;
        }
    }
    textureStore(fluence, vec3<i32>(id), vec4<f32>(light, 0.0));
}
