// Bounced light (`bounce.rs`): what every bounce shader and the scene pass
// share.

// A piece of a big surface: where light is taken in and sent back out
// (`Disc` in `bounce/bake.rs`).
struct BounceDisc {
    pos: vec3<f32>,
    radius: f32,
    normal: vec3<f32>,
    area: f32,
    matte: vec3<f32>,
    patch_index: u32,
    f0: vec3<f32>,
    roughness: f32,
    // Planes facing the disc: a point behind one is on the other side of
    // something. A plane with no normal clips nothing.
    clip0: vec4<f32>,
    clip1: vec4<f32>,
    reach: f32,
    // 1 on the ground.
    ground: f32,
    pad0: f32,
    pad1: f32,
};

// `Field` in `bounce.rs`.
struct BounceField {
    // xyz: the corner of the grid that finds discs. w: its cell, metres.
    origin: vec4<f32>,
    // xyz: its cells along each axis. w: 1 when the bounce is live.
    dims: vec4<f32>,
    // xyz: the first air point. w: metres between air points.
    air_origin: vec4<f32>,
    // xyz: air points along each axis. w: output pixels per texel of the
    // low pictures, along each axis.
    air_dims: vec4<f32>,
    // x: discs. y: patches. z: groups. w: air points.
    counts: vec4<u32>,
    // xyz: the eye. w: how much an emitter's size widens its highlight.
    eye: vec4<f32>,
};

/// A unit direction as one number a half float holds exactly: 32 steps on
/// each axis of its octahedral square. Coarse, and enough to tell a floor
/// from a wall.
fn bounce_pack_facing(d: vec3<f32>) -> f32 {
    let n = d / (abs(d.x) + abs(d.y) + abs(d.z));
    var e = n.xy;
    if n.z < 0.0 {
        e = (1.0 - abs(n.yx)) * vec2<f32>(select(-1.0, 1.0, n.x >= 0.0), select(-1.0, 1.0, n.y >= 0.0));
    }
    let q = round(saturate(e * 0.5 + 0.5) * 31.0);
    return q.x + q.y * 32.0;
}

fn bounce_unpack_facing(v: f32) -> vec3<f32> {
    let q = vec2<f32>(v % 32.0, floor(v / 32.0));
    let e = q / 31.0 * 2.0 - 1.0;
    var n = vec3<f32>(e, 1.0 - abs(e.x) - abs(e.y));
    if n.z < 0.0 {
        let s = vec2<f32>(select(-1.0, 1.0, n.x >= 0.0), select(-1.0, 1.0, n.y >= 0.0));
        n = vec3<f32>((1.0 - abs(n.yx)) * s, n.z);
    }
    return normalize(n);
}

/// A unit direction from 32 bits (`pack_direction` in `bounce/bake.rs`).
fn bounce_unpack_direction(v: u32) -> vec3<f32> {
    let e = vec2<f32>(f32(v & 0xffffu), f32(v >> 16u)) / 65535.0 * 2.0 - 1.0;
    var n = vec3<f32>(e, 1.0 - abs(e.x) - abs(e.y));
    if n.z < 0.0 {
        let s = vec2<f32>(select(-1.0, 1.0, n.x >= 0.0), select(-1.0, 1.0, n.y >= 0.0));
        n = vec3<f32>((1.0 - abs(n.yx)) * s, n.z);
    }
    return normalize(n);
}
