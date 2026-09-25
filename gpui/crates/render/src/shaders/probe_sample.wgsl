// The scene pass's view of the reflection probes (`probe_common.wgsl`).
//
// A point between probes blends the eight around it by their trilinear
// weights, less a small floor so a far corner drops out without a step, and
// each looks its reflection up at the ray's hit on the parallax box as that
// probe sees it: a truss a metre from a probe reflects the deck under
// itself, not the deck under the probe. Past the grid's edge the probes fade
// out over `box_min.w` metres and the sky probe takes over.

@group(2) @binding(6) var probe_cubes: texture_cube_array<f32>;
@group(2) @binding(7) var<uniform> probe_grid: ProbeGrid;
// Each probe face's mean radiance, six to a probe (`probe_ambient` in
// `probe_filter.wgsl`): the diffuse, as an ambient cube.
@group(2) @binding(8) var<uniform> probe_ambient: array<vec4<f32>, PROBE_AMBIENT_LEN>;

/// A probe's ambient cube along the unit cube-space direction `c`.
fn probe_ambient_at(index: u32, c: vec3<f32>) -> vec3<f32> {
    let w = c * c;
    let base = index * 6u;
    return w.x * probe_ambient[base + select(1u, 0u, c.x >= 0.0)].rgb
        + w.y * probe_ambient[base + select(3u, 2u, c.y >= 0.0)].rgb
        + w.z * probe_ambient[base + select(5u, 4u, c.z >= 0.0)].rgb;
}

struct ProbeLight {
    // Prefiltered radiance along the reflection, for the split sum.
    specular: vec3<f32>,
    // Mean radiance over the normal's side: E / pi, as the sky probe's
    // irradiance cube stores it.
    diffuse: vec3<f32>,
    // How much the probes stand in for the sky probe here, 0 to 1.
    weight: f32,
};

/// The direction from probe `at` to where the ray from `world` along `r`
/// leaves the parallax box. A point outside the box keeps `r`.
fn probe_parallax(world: vec3<f32>, r: vec3<f32>, at: vec3<f32>) -> vec3<f32> {
    let lo = probe_grid.box_min.xyz;
    let hi = probe_grid.box_max.xyz;
    if any(world < lo) || any(world > hi) {
        return r;
    }
    let safe = select(r, vec3<f32>(1e-6), abs(r) < vec3<f32>(1e-6));
    let far = max((hi - world) / safe, (lo - world) / safe);
    let t = min(min(far.x, far.y), far.z);
    return world + r * t - at;
}

fn probe_light(world: vec3<f32>, n: vec3<f32>, r: vec3<f32>, roughness: f32) -> ProbeLight {
    var result: ProbeLight;
    result.specular = vec3<f32>(0.0);
    result.diffuse = vec3<f32>(0.0);
    result.weight = 0.0;
    if probe_grid.origin.w < 0.5 {
        return result;
    }
    let top = probe_grid.dims.xyz - 1.0;
    let g = (world - probe_grid.origin.xyz) / probe_grid.step.xyz;
    // Metres past the grid's outer probes, less half a cell, across the
    // ground only: a truss high over the grid is still lit by it.
    let outside = max(max(-g.xy, g.xy - top.xy) - 0.5, vec2<f32>(0.0)) * probe_grid.step.xy;
    let fade = 1.0 - smoothstep(0.0, probe_grid.box_min.w, length(outside));
    if fade <= 0.0 {
        return result;
    }
    let c = clamp(g, vec3<f32>(0.0), top);
    let cell = min(floor(c), max(top - 1.0, vec3<f32>(0.0)));
    let f = c - cell;
    let base = probe_grid.step.w;
    let mips = probe_grid.dims.w;
    let specular_lod = base + roughness * (mips - 1.0);
    let n_cube = probe_cube_direction(n);
    // Corner weights before and after the visibility test: a point the
    // probes cannot see (under a deck they stand over) is lit by what is
    // left, and by the sky probe's occluded light where nothing is.
    var total = 0.0;
    var reach = 0.0;
    // Off the surface, so the surface's own texel does not hide it.
    let lifted = world + n * 0.1;
    for (var corner = 0u; corner < 8u; corner++) {
        let offset = vec3<f32>(f32(corner & 1u), f32((corner >> 1u) & 1u), f32((corner >> 2u) & 1u));
        let axis = mix(1.0 - f, f, offset);
        // The floor keeps a corner that has almost no share from costing
        // two samples; subtracting it, not cutting at it, keeps the blend
        // continuous as the point moves.
        var w = max(axis.x * axis.y * axis.z - 0.02, 0.0);
        if w <= 0.0 || any(cell + offset > top) {
            continue;
        }
        let index = probe_index(probe_grid, vec3<u32>(cell + offset));
        let at = probe_grid.positions[index].xyz;
        reach += w;
        // Does the probe see this point? Compare how far it saw toward the
        // point with how far the point is. The margin is a texel's spread
        // at that distance and some, soft over half a metre.
        let toward = lifted - at;
        let far = length(toward);
        let seen = textureSampleLevel(probe_cubes, environment_sampler, probe_cube_direction(toward), index, base).a;
        let visible = saturate((seen + 0.25 + 0.1 * far - far) / 0.5);
        w *= visible;
        if w <= 0.0 {
            continue;
        }
        let d = probe_cube_direction(probe_parallax(world, r, at));
        result.specular += w * textureSampleLevel(probe_cubes, environment_sampler, d, index, specular_lod).rgb;
        result.diffuse += w * probe_ambient_at(index, n_cube);
        total += w;
    }
    if total <= 0.0 {
        return result;
    }
    result.specular /= total;
    result.diffuse /= total;
    result.weight = fade * saturate(total / max(reach, 1e-6));
    return result;
}
