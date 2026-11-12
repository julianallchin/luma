// Bounced light for the picture: at every few pixels, the discs round the
// surface there that lie on it, blended. The scene pass spreads these over
// the pixels between (`bounce_sample.wgsl`). Prefixed by
// `bounce_common.wgsl`; `bounce_lookup.wgsl` follows its bindings.
//
// matte: rgb the bounced light landing on the surface, W/m2; a the
//   distance along the camera's forward axis, or less than nothing where no
//   disc speaks for the surface (a thin thing, the sky).
// mirror: rgb the bounced light the surface mirrors toward the eye; a the
//   way the surface faces (`bounce_pack_facing`).

// `AoParams` in `ambient_occlusion.wgsl`: the camera the depth was drawn
// from.
struct BounceCamera {
    camera: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    forward: vec4<f32>,
    viewport: vec4<f32>,
    texels: vec4<f32>,
};

@group(0) @binding(0) var<uniform> bounce_field: BounceField;
@group(0) @binding(1) var<uniform> view: BounceCamera;
@group(0) @binding(2) var depth_tex: texture_depth_2d;
@group(0) @binding(3) var<storage, read> bounce_discs: array<BounceDisc>;
@group(0) @binding(4) var<storage, read> bounce_cells: array<u32>;
@group(0) @binding(5) var<storage, read> bounce_landed: array<vec4<f32>>;
@group(0) @binding(6) var matte_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(7) var mirror_out: texture_storage_2d<rgba16float, write>;

/// Distance along the camera's forward axis for a reverse-Z depth sample.
fn view_distance(raw: f32) -> f32 {
    let near = view.viewport.z;
    let far = view.viewport.w;
    return near * far / max(near + raw * (far - near), 1e-7);
}

fn world_at(pixel: vec2<i32>) -> vec3<f32> {
    let clamped = clamp(pixel, vec2<i32>(0), vec2<i32>(view.viewport.xy) - 1);
    let raw = textureLoad(depth_tex, clamped, 0);
    let p = vec2<f32>(clamped) + 0.5;
    let ndc = vec2<f32>(p.x * 2.0 / view.viewport.x - 1.0, 1.0 - p.y * 2.0 / view.viewport.y);
    let ray = view.forward.xyz + view.right.xyz * (ndc.x * view.right.w) + view.up.xyz * (ndc.y * view.up.w);
    return view.camera.xyz + ray * view_distance(max(raw, 1e-7));
}

@compute @workgroup_size(8, 8)
fn bounce_low(@builtin(global_invocation_id) id: vec3<u32>) {
    let step = i32(bounce_field.air_dims.w);
    let pixel = vec2<i32>(id.xy) * step + step / 2;
    if pixel.x >= i32(view.viewport.x) + step || pixel.y >= i32(view.viewport.y) + step {
        return;
    }
    let texel = vec2<i32>(id.xy);
    let centre = clamp(pixel, vec2<i32>(0), vec2<i32>(view.viewport.xy) - 1);
    var matte = vec4<f32>(0.0, 0.0, 0.0, -1.0);
    var mirror = vec4<f32>(0.0);
    if textureLoad(depth_tex, centre, 0) > 0.0 {
        let p = world_at(centre);
        // The surface's facing from its neighbours in the depth: on each
        // axis the nearer of the two, so an edge does not tilt it.
        let left = world_at(centre - vec2<i32>(1, 0)) - p;
        let right = world_at(centre + vec2<i32>(1, 0)) - p;
        let above = world_at(centre - vec2<i32>(0, 1)) - p;
        let below = world_at(centre + vec2<i32>(0, 1)) - p;
        var across = right;
        if dot(left, left) < dot(right, right) { across = -left; }
        var down = below;
        if dot(above, above) < dot(below, below) { down = -above; }
        var n = normalize(cross(down, across));
        if dot(n, view.camera.xyz - p) < 0.0 { n = -n; }
        let found = bounce_at(p, n);
        if found.weight > 0.0 {
            matte = vec4<f32>(found.matte, dot(p - view.camera.xyz, view.forward.xyz));
            mirror = vec4<f32>(found.mirror, bounce_pack_facing(n));
        }
    }
    textureStore(matte_out, texel, matte);
    textureStore(mirror_out, texel, mirror);
}
