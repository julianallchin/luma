// Bounced light for the picture: at every few pixels, the discs round the
// surface there that lie on it, blended. The scene pass spreads these over
// the pixels between (`bounce_sample.wgsl`). Prefixed by
// `bounce_common.wgsl`.
//
// matte: rgb the bounced light landing on the surface, W/m2; a the
//   distance along the camera's forward axis, or less than nothing where no
//   disc speaks for the surface (a thin thing, the sky).
// mirror: rgb the bounced light the surface mirrors toward the eye.

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

@group(0) @binding(0) var<uniform> field: BounceField;
@group(0) @binding(1) var<uniform> view: BounceCamera;
@group(0) @binding(2) var depth_tex: texture_depth_2d;
@group(0) @binding(3) var<storage, read> discs: array<BounceDisc>;
@group(0) @binding(4) var<storage, read> cell_start: array<u32>;
@group(0) @binding(5) var<storage, read> cell_items: array<u32>;
@group(0) @binding(6) var<storage, read> gathered: array<vec4<f32>>;
@group(0) @binding(7) var<storage, read> mirrored: array<vec4<f32>>;
@group(0) @binding(8) var matte_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(9) var mirror_out: texture_storage_2d<rgba16float, write>;

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

/// Whether `p` is on the disc's side of everything that stands by it.
fn same_side(disc: BounceDisc, p: vec3<f32>) -> bool {
    return dot(disc.clip0.xyz, p) - disc.clip0.w >= -0.01
        && dot(disc.clip1.xyz, p) - disc.clip1.w >= -0.01;
}

@compute @workgroup_size(8, 8)
fn bounce_low(@builtin(global_invocation_id) id: vec3<u32>) {
    let step = i32(field.air_dims.w);
    let pixel = vec2<i32>(id.xy) * step + step / 2;
    if pixel.x >= i32(view.viewport.x) + step || pixel.y >= i32(view.viewport.y) + step {
        return;
    }
    let texel = vec2<i32>(id.xy);
    let centre = clamp(pixel, vec2<i32>(0), vec2<i32>(view.viewport.xy) - 1);
    let raw = textureLoad(depth_tex, centre, 0);
    var matte = vec4<f32>(0.0, 0.0, 0.0, -1.0);
    var mirror = vec4<f32>(0.0);
    let cell = vec3<i32>(floor((world_at(centre) - field.origin.xyz) / field.origin.w));
    let dims = vec3<i32>(field.dims.xyz);
    if raw > 0.0 && field.dims.w > 0.5 && all(cell >= vec3<i32>(0)) && all(cell < dims) {
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
        let lifted = p + n * 0.05;
        let index = u32(cell.x + dims.x * (cell.y + dims.y * cell.z));
        var total = 0.0;
        var light = vec3<f32>(0.0);
        var shine = vec3<f32>(0.0);
        for (var k = cell_start[index]; k < cell_start[index + 1u]; k++) {
            let j = cell_items[k];
            let disc = discs[j];
            let d = p - disc.pos;
            let t = 1.0 - dot(d, d) / (disc.reach * disc.reach);
            if t <= 0.0 || !same_side(disc, lifted) { continue; }
            let facing = max(dot(n, disc.normal), 0.0);
            let plane = saturate(1.0 - abs(dot(d, disc.normal)) / max(0.35 * disc.reach, 0.05));
            // Small discs speak louder where they are.
            let w = t * t / (disc.reach * disc.reach) * facing * facing * facing * facing * plane;
            light += w * gathered[j].rgb;
            shine += w * mirrored[j].rgb;
            total += w;
        }
        if total > 1e-3 {
            matte = vec4<f32>(light / total, dot(p - view.camera.xyz, view.forward.xyz));
            mirror = vec4<f32>(shine / total, 1.0);
        }
    }
    textureStore(matte_out, texel, matte);
    textureStore(mirror_out, texel, mirror);
}
