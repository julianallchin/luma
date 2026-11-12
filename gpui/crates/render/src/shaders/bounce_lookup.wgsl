// The bounced light at a point on a big surface: the discs round it that
// lie on it, blended (`bounce.rs`). Shared by the low pictures
// (`bounce_low.wgsl`) and the scene pass (`bounce_sample.wgsl`), each of
// which binds `bounce_field`, `bounce_discs`, `bounce_cells` and
// `bounce_landed` in its own group first.
//
// `bounce_cells` is the grid that finds discs: each cell's start in the
// list, one more than the cells, then the list. `bounce_landed` holds two
// values a disc: the bounced light landing on it, W/m2, and the bounced
// light it mirrors toward the eye.

struct BounceFound {
    matte: vec3<f32>,
    mirror: vec3<f32>,
    // Nothing where no disc speaks for the surface: a thin thing, an edge.
    weight: f32,
};

/// Whether `p` is on the disc's side of everything that stands by it.
fn bounce_same_side(disc: BounceDisc, p: vec3<f32>) -> bool {
    return dot(disc.clip0.xyz, p) - disc.clip0.w >= -0.01
        && dot(disc.clip1.xyz, p) - disc.clip1.w >= -0.01;
}

fn bounce_at(p: vec3<f32>, n: vec3<f32>) -> BounceFound {
    var found: BounceFound;
    found.matte = vec3<f32>(0.0);
    found.mirror = vec3<f32>(0.0);
    found.weight = 0.0;
    let dims = vec3<i32>(bounce_field.dims.xyz);
    let cell = vec3<i32>(floor((p - bounce_field.origin.xyz) / bounce_field.origin.w));
    if bounce_field.dims.w < 0.5 || any(cell < vec3<i32>(0)) || any(cell >= dims) {
        return found;
    }
    let index = u32(cell.x + dims.x * (cell.y + dims.y * cell.z));
    let list = u32(dims.x * dims.y * dims.z) + 1u;
    let lifted = p + n * 0.05;
    for (var k = bounce_cells[index]; k < bounce_cells[index + 1u]; k++) {
        let j = bounce_cells[list + k];
        let disc = bounce_discs[j];
        let d = p - disc.pos;
        let t = 1.0 - dot(d, d) / (disc.reach * disc.reach);
        if t <= 0.0 || !bounce_same_side(disc, lifted) { continue; }
        let facing = max(dot(n, disc.normal), 0.0);
        let plane = saturate(1.0 - abs(dot(d, disc.normal)) / max(0.35 * disc.reach, 0.05));
        // Small discs speak louder where they are.
        let w = t * t / (disc.reach * disc.reach) * facing * facing * facing * facing * plane;
        found.matte += w * bounce_landed[j * 2u].rgb;
        found.mirror += w * bounce_landed[j * 2u + 1u].rgb;
        found.weight += w;
    }
    if found.weight > 1e-3 {
        found.matte /= found.weight;
        found.mirror /= found.weight;
    } else {
        found.weight = 0.0;
    }
    return found;
}
