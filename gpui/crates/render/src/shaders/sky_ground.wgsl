// Passes that run only when the stage or the sun changes: the height field
// itself, and what the ground under the stage receives.
//
// Prefixed by `sky_height.wgsl`.

// Layer 0 looked down (depth = z / top), layer 1 looked up
// (depth = (top - z) / top). Cleared to zero: bare ground, nothing overhead.
@group(0) @binding(2) var height_depth: texture_depth_2d_array;
@group(0) @binding(3) var heights_out: texture_storage_2d<rg32float, write>;
@group(0) @binding(4) var ground_out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(5) var mip_src: texture_2d<f32>;

@compute @workgroup_size(8, 8, 1)
fn convert(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(vec2<f32>(id.xy) >= height.size.xy) {
        return;
    }
    let top_z = height.origin.w;
    let above = textureLoad(height_depth, vec2<i32>(id.xy), 0, 0);
    let below = textureLoad(height_depth, vec2<i32>(id.xy), 1, 0);
    let bottom = select(NO_SLAB, (1.0 - below) * top_z, below > 0.0);
    textureStore(heights_out, vec2<i32>(id.xy), vec4<f32>(above * top_z, bottom, 0.0, 0.0));
}

/// Whether the sun reaches the ground at `xy`, marched through the slabs.
fn ground_sun(xy: vec2<f32>) -> f32 {
    let sun = height.sun.xyz;
    if height.sun.w < 0.5 || sun.z <= 0.0 {
        return 1.0;
    }
    let bias = height.march.z;
    // Anything standing on this very point covers it.
    let own = slab_at(xy);
    if own.y <= bias && own.x > bias {
        return 0.0;
    }
    let across = length(sun.xy);
    if across < 1e-4 {
        return select(1.0, 0.0, own.x > bias);
    }
    let direction = sun.xy / across;
    let rise = sun.z / across;
    let reach = min(height.origin.w / rise, 2.0 * max(height.size.x, height.size.y) * height.origin.z);
    let steps = 96u;
    let step = max(height.origin.z, reach / f32(steps));
    for (var i = 1u; i <= steps; i = i + 1u) {
        let t = f32(i) * step;
        if t > reach + step {
            break;
        }
        let z = t * rise;
        let slab = slab_at(xy + direction * t);
        if slab.y <= z && slab.x >= z {
            return 0.0;
        }
    }
    return 1.0;
}

/// One ground texel: r = sun visibility, g = sky visibility of the bare
/// ground under whatever the stage puts over it.
@compute @workgroup_size(8, 8, 1)
fn ground(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(ground_out);
    if any(id.xy >= size) {
        return;
    }
    let xy = height.origin.xy + (vec2<f32>(id.xy) + 0.5) * height.size.z;
    let sky = sky_visibility(vec3<f32>(xy, 0.0), vec3<f32>(0.0, 0.0, 1.0), 0.5);
    textureStore(ground_out, vec2<i32>(id.xy), vec4<f32>(ground_sun(xy), sky, 0.0, 1.0));
}

/// Box-filtered mip: the ground bounce reads the level whose texel matches
/// how far below it the ground is.
@compute @workgroup_size(8, 8, 1)
fn mip(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(ground_out);
    if any(id.xy >= size) {
        return;
    }
    let source = vec2<i32>(textureDimensions(mip_src)) - 1;
    let base = vec2<i32>(id.xy) * 2;
    var sum = vec4<f32>(0.0);
    for (var j = 0; j < 2; j = j + 1) {
        for (var i = 0; i < 2; i = i + 1) {
            sum += textureLoad(mip_src, min(base + vec2<i32>(i, j), source), 0);
        }
    }
    textureStore(ground_out, vec2<i32>(id.xy), sum * 0.25);
}
