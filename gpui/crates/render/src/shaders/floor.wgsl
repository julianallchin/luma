// The ground's material (`floor.rs`). Appended to `scene.wgsl`; `fs_ground`
// builds a `Surface` and hands it to the same `shade` every mesh uses, so the
// floor takes the sun, its shadows, the clouds, the fixtures and the air
// exactly as the flat ground did.
//
// The maps come in on the material group's slots, in the floor's own roles:
//   base_color_map  sRGB albedo, height in alpha
//   normal_map      normal X and Y (OpenGL, +Y up), roughness, occlusion
//   emissive_map    the transition set's albedo and height, as the first
//   occlusion_map   the transition set's normal, roughness and occlusion
//
// Five problems, one answer each:
//
// Tiling. A tile is 0.75 to 4.6 m and the camera sees 10 to 50 m of floor, so
// plain repetition draws a grid. Every map is read through Mikkelsen's
// hex-tiling ("Practical Real-Time Hex-Tiling", JCGT 2022): three copies of
// the texture, each on a cell of a hexagonal lattice with its own random
// offset and rotation (none for sets with a direction, such as wind
// ripples: `max_rotation_deg`), blended by barycentric weight sharpened toward the
// brighter copy so the seams read as texture rather than as a cross-fade.
// Normals are blended as slopes after each copy's rotation is undone. Planks
// have a grain with a direction and are laid plainly. Every set, and its
// transition set, is read in world metres over its own `tile_m`, the size
// its source states (`floors.json`).
//
// Scale above the tile. Value noise at 5 to 15 m and 30 to 100 m moves the
// ground between its set's two tints (dry and lush grass, damp and dry
// earth, stained and worn concrete), mottles it over a few metres and wears
// its roughness; on grass and dirt it also decides where a second material
// (mud and sprouts, or moss) breaks through. Heights decide the edge between
// the two, so a patch ends along the grain of the ground and not along a
// noise contour.
//
// Roughness. A scan's roughness is one blade's or grain's. A field of them,
// seen from metres away, scatters light over every angle its blades face, so
// each set has a least roughness it is never drawn under.
//
// Distance. Once a whole tile is a pixel or two the texture is nothing but
// its own average, and anything else shimmers. The maps fade to their last
// mip, the material's mean, and past that no copy is read at all. The
// variation above the tile is applied after the fade, so it stays.
//
// Depth. Dirt, sand and gravel near the camera are marched as a height field
// (parallax occlusion mapping) through the same three copies.
//
// Wet ground and grass blades are not drawn. Both would start from the
// `Surface` `ground_surface` returns: wetness darkens its albedo and smooths
// its roughness, and blades would stand on its height and coverage.

const HEX_SCALE: f32 = 3.4641016; // 2 sqrt(3): about two cells per tile.
const HEX_EXPONENT: f32 = 7.0;
const HEX_CONTRAST: f32 = 0.6;

struct Hex {
    // Each copy's texture coordinates at the unshifted point, and its
    // rotation. A shifted point `st + d` reads copy `i` at
    // `uv[i] + rot[i] * d`.
    uv0: vec2<f32>,
    uv1: vec2<f32>,
    uv2: vec2<f32>,
    rot0: mat2x2<f32>,
    rot1: mat2x2<f32>,
    rot2: mat2x2<f32>,
    // Barycentric weights before the contrast step.
    w: vec3<f32>,
};

fn hex_hash(p: vec2<f32>) -> vec2<f32> {
    let r = mat2x2<f32>(127.1, 311.7, 269.5, 183.3) * p;
    return fract(sin(r) * 43758.5453);
}

/// A cell's turn: a hashed angle within `spread` of a half turn either way.
fn hex_rotation(vertex: vec2<f32>, spread: f32) -> mat2x2<f32> {
    var angle = abs(vertex.x * vertex.y) + abs(vertex.x + vertex.y) + PI;
    angle = angle - 2.0 * PI * floor(angle / (2.0 * PI));
    if angle > PI {
        angle -= 2.0 * PI;
    }
    angle *= spread;
    let c = cos(angle);
    let s = sin(angle);
    return mat2x2<f32>(c, s, -s, c);
}

/// The three hex-lattice copies covering texture coordinate `st`, turned
/// by at most `spread` of a half turn, or with plain tiling one unturned
/// copy.
fn hex_grid(st: vec2<f32>, spread: f32) -> Hex {
    if globals.floor_b.z > 0.5 {
        var plain: Hex;
        plain.w = vec3<f32>(1.0, 0.0, 0.0);
        plain.rot0 = mat2x2<f32>(1.0, 0.0, 0.0, 1.0);
        plain.rot1 = plain.rot0;
        plain.rot2 = plain.rot0;
        plain.uv0 = st;
        plain.uv1 = st;
        plain.uv2 = st;
        return plain;
    }
    let grid = st * HEX_SCALE;
    // Skew the square grid into a triangular one.
    let skewed = vec2<f32>(grid.x - 0.57735027 * grid.y, 1.15470054 * grid.y);
    let base = floor(skewed);
    let f = fract(skewed);
    let z = 1.0 - f.x - f.y;
    let s = select(0.0, 1.0, z < 0.0);
    let s2 = 2.0 * s - 1.0;
    var hex: Hex;
    hex.w = vec3<f32>(-z * s2, s - f.y * s2, s - f.x * s2);
    let v0 = base + vec2<f32>(s, s);
    let v1 = base + vec2<f32>(s, 1.0 - s);
    let v2 = base + vec2<f32>(1.0 - s, s);
    hex.rot0 = hex_rotation(v0, spread);
    hex.rot1 = hex_rotation(v1, spread);
    hex.rot2 = hex_rotation(v2, spread);
    hex.uv0 = hex_copy(st, v0, hex.rot0);
    hex.uv1 = hex_copy(st, v1, hex.rot1);
    hex.uv2 = hex_copy(st, v2, hex.rot2);
    return hex;
}

fn hex_copy(st: vec2<f32>, vertex: vec2<f32>, rot: mat2x2<f32>) -> vec2<f32> {
    // The lattice vertex back in unskewed texture space.
    let centre = vec2<f32>(vertex.x + 0.5 * vertex.y, 0.8660254 * vertex.y) / HEX_SCALE;
    return rot * (st - centre) + centre + hex_hash(vertex);
}

/// The copies' final weights: barycentric, sharpened toward the copy whose
/// sample is brighter, so one copy wins in each spot instead of three
/// cross-fading into mush.
fn hex_weights(hex: Hex, l: vec3<f32>) -> vec3<f32> {
    let d = mix(vec3<f32>(1.0), l, HEX_CONTRAST);
    let w = d * pow(hex.w, vec3<f32>(HEX_EXPONENT));
    return w / max(w.x + w.y + w.z, 1e-6);
}

struct GroundSample {
    albedo: vec3<f32>,
    height: f32,
    // Height-field slopes in world XY: dh/dx, dh/dy.
    slope: vec2<f32>,
    roughness: f32,
    ao: f32,
    // Mean length of the filtered normals: under one, the pixel covers
    // bumps the normal alone cannot show (`toksvig` below).
    normal_length: f32,
};

/// A world-slope from one copy's normal texel, rotated back into world XY.
fn hex_slope(texel: vec4<f32>, rot: mat2x2<f32>) -> vec3<f32> {
    let xy = texel.xy * 2.0 - 1.0;
    let z = sqrt(saturate(1.0 - dot(xy, xy)));
    // The copy reads `rot * st`, so its slopes turn back by the transpose.
    // Texture space runs down the image and the normal's +Y up it; the two
    // flips around that transpose make it `rot` itself.
    let slope = rot * (-xy / max(z, 0.05));
    return vec3<f32>(slope, length(vec3<f32>(xy, z)));
}

/// One material set, read through hex tiling at `hex` with world-scale
/// gradients `gx` and `gy` (texture units per pixel), shifted by `shift`.
fn ground_set(
    albedo_map: texture_2d<f32>,
    detail_map: texture_2d<f32>,
    hex: Hex,
    shift: vec2<f32>,
    gx: vec2<f32>,
    gy: vec2<f32>,
) -> GroundSample {
    let u0 = hex.uv0 + hex.rot0 * shift;
    let u1 = hex.uv1 + hex.rot1 * shift;
    let u2 = hex.uv2 + hex.rot2 * shift;
    let a0 = textureSampleGrad(albedo_map, material_sampler, u0, hex.rot0 * gx, hex.rot0 * gy);
    let a1 = textureSampleGrad(albedo_map, material_sampler, u1, hex.rot1 * gx, hex.rot1 * gy);
    let a2 = textureSampleGrad(albedo_map, material_sampler, u2, hex.rot2 * gx, hex.rot2 * gy);
    let d0 = textureSampleGrad(detail_map, material_sampler, u0, hex.rot0 * gx, hex.rot0 * gy);
    let d1 = textureSampleGrad(detail_map, material_sampler, u1, hex.rot1 * gx, hex.rot1 * gy);
    let d2 = textureSampleGrad(detail_map, material_sampler, u2, hex.rot2 * gx, hex.rot2 * gy);
    let luma = vec3<f32>(0.2126, 0.7152, 0.0722);
    let w = hex_weights(hex, vec3<f32>(dot(a0.rgb, luma), dot(a1.rgb, luma), dot(a2.rgb, luma)));
    let s0 = hex_slope(d0, hex.rot0);
    let s1 = hex_slope(d1, hex.rot1);
    let s2 = hex_slope(d2, hex.rot2);
    var out: GroundSample;
    out.albedo = w.x * a0.rgb + w.y * a1.rgb + w.z * a2.rgb;
    out.height = w.x * a0.a + w.y * a1.a + w.z * a2.a;
    out.slope = w.x * s0.xy + w.y * s1.xy + w.z * s2.xy;
    out.normal_length = w.x * s0.z + w.y * s1.z + w.z * s2.z;
    out.roughness = w.x * d0.b + w.y * d1.b + w.z * d2.b;
    out.ao = w.x * d0.a + w.y * d1.a + w.z * d2.a;
    return out;
}

/// The set's own mean: its last mip, read anywhere.
fn ground_mean(albedo_map: texture_2d<f32>, detail_map: texture_2d<f32>) -> GroundSample {
    let a = textureSampleLevel(albedo_map, material_sampler, vec2<f32>(0.5), 16.0);
    let d = textureSampleLevel(detail_map, material_sampler, vec2<f32>(0.5), 16.0);
    var out: GroundSample;
    out.albedo = a.rgb;
    out.height = a.a;
    out.slope = vec2<f32>(0.0);
    out.normal_length = 1.0;
    out.roughness = d.b;
    out.ao = d.a;
    return out;
}

fn ground_mix(a: GroundSample, b: GroundSample, t: f32) -> GroundSample {
    var out: GroundSample;
    out.albedo = mix(a.albedo, b.albedo, t);
    out.height = mix(a.height, b.height, t);
    out.slope = mix(a.slope, b.slope, t);
    out.normal_length = mix(a.normal_length, b.normal_length, t);
    out.roughness = mix(a.roughness, b.roughness, t);
    out.ao = mix(a.ao, b.ao, t);
    return out;
}

fn value_hash(p: vec2<f32>) -> f32 {
    let q = fract(p * vec2<f32>(0.1031, 0.1030));
    let r = q + dot(q, q.yx + 33.33);
    return fract((r.x + r.y) * r.x);
}

/// Smooth value noise, 0 to 1.
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = value_hash(i);
    let b = value_hash(i + vec2<f32>(1.0, 0.0));
    let c = value_hash(i + vec2<f32>(0.0, 1.0));
    let d = value_hash(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

/// Two octaves of value noise at `scale` metres, 0 to 1.
fn macro_noise(world: vec2<f32>, scale: f32, seed: f32) -> f32 {
    let p = world / scale + vec2<f32>(seed * 17.0, seed * 31.0);
    return 0.65 * value_noise(p) + 0.35 * value_noise(p * 2.7 + 5.3);
}

/// Height band over which the transition set and the ground blend.
const PATCH_BAND: f32 = 0.3;

/// The share of the ground the height blend gives the transition set at
/// `breakthrough`, for heights spread evenly: the transition shows where
/// its height beats the ground's by `1 - 2 breakthrough`, and the
/// difference of two even heights is triangular.
fn patch_cover(breakthrough: f32) -> f32 {
    let c = 1.0 - 2.0 * breakthrough;
    return select(1.0 - 0.5 * (1.0 + c) * (1.0 + c), 0.5 * (1.0 - c) * (1.0 - c), c >= 0.0);
}

/// Value noise summed over three octaves from `scale` metres down, 0 to 1.
/// An octave goes to its mean, one half, as the pixel's footprint (`pixel`,
/// metres) nears its size: sampled at one point it would alias, and a row
/// of distant ground would take one random tint.
fn field_noise(world: vec2<f32>, scale: f32, seed: f32, pixel: f32) -> f32 {
    let p = world / scale + vec2<f32>(seed * 13.0, seed * 29.0);
    let r = pixel / scale;
    let o1 = mix(value_noise(p), 0.5, smoothstep(0.15, 0.6, r));
    let o2 = mix(value_noise(p * 2.13 + 3.1), 0.5, smoothstep(0.15, 0.6, r * 2.13));
    let o3 = mix(value_noise(p * 4.37 + 7.7), 0.5, smoothstep(0.15, 0.6, r * 4.37));
    return 0.57 * o1 + 0.29 * o2 + 0.14 * o3;
}

/// Toksvig: a filtered normal shorter than one stands for slopes the pixel
/// averaged away, and they widen the specular lobe.
fn toksvig(roughness: f32, normal_length: f32) -> f32 {
    let l = clamp(normal_length, 0.05, 1.0);
    let alpha = roughness * roughness;
    return sqrt(sqrt(saturate(alpha * alpha + (1.0 - l) / l)));
}

// globals.floor (`floor.rs::Surface`):
// a: x tile, metres. y parallax depth, metres (zero: none). z unevenness,
//    a GGX alpha. w strength of the macro variation.
// b: x transition tile, metres (zero: none). y share of the ground the
//    transition covers. z tiling: 0 hex copies, 1 plain with no macro
//    variation (the picture the anti-tiling is measured against), 2 plain
//    with the variation (planks). w the least roughness.
// c: rgb the first tint. w the most a hex copy turns, as a share of a
//    half turn.
// d: rgb the second tint. w the same for the transition set.

@fragment
fn fs_ground(in: VsOut) -> @location(0) vec4<f32> {
    let a = globals.floor_a;
    let b = globals.floor_b;
    let dx = dpdx(in.world);
    let dy = dpdy(in.world);
    // The ground the pixel covers, not a point behind the camera where a
    // horizon pixel's centre extrapolates to (`ground_fragment`).
    let at = ground_fragment(in.world);
    let world = at.xy;
    let distance = length(at - globals.camera_pos.xyz);
    // Texture space: one unit per tile, with image rows running down so the
    // OpenGL normal's +Y is world +Y.
    let flip = vec2<f32>(1.0, -1.0);
    let st = world * flip / a.x;
    let gx = dx.xy * flip / a.x;
    let gy = dy.xy * flip / a.x;

    // Detail fades to the mean only where a whole tile is a pixel or two
    // across: the footprint's long side, in tiles. Nearer than that the mips
    // already average it; starting sooner flattened the ground from an
    // ordinary orbit height.
    let footprint = max(length(gx), length(gy));
    let fade = smoothstep(0.35, 1.0, footprint);

    let mean = ground_mean(base_color_map, normal_map);
    let has_transition = b.x > 0.0;
    // Where the transition set breaks through: a noise field over a few
    // metres, its coordinates bent by a smaller one so the edges wander,
    // thresholded to the set's share of the ground. Near, the heights decide
    // which set shows (below); far, the two means mix by the share of the
    // pixel the heights would give the transition, so the two agree and a
    // patch has no rim where one gives way to the other.
    var breakthrough = 0.0;
    var far = mean;
    if has_transition {
        let warp = vec2<f32>(macro_noise(world, 2.3, 5.0), macro_noise(world, 2.3, 6.0)) - 0.5;
        let field = macro_noise(world + 3.0 * warp, 9.0, 3.0);
        breakthrough = smoothstep(1.0 - b.y - 0.25, 1.0 - b.y + 0.25, field);
        far = ground_mix(mean, ground_mean(emissive_map, occlusion_map), patch_cover(breakthrough));
    }

    var ground = far;
    if fade < 1.0 {
        let hex = hex_grid(st, globals.floor_c.w);
        // Parallax: march the height field along the view, near the camera
        // only. Dirt, sand and gravel only; `a.y` is zero for the rest and on
        // `Quality::Low`.
        var shift = vec2<f32>(0.0);
        let depth = a.y * (1.0 - smoothstep(6.0, 12.0, distance));
        if depth > 0.0 {
            let view = normalize(globals.camera_pos.xyz - at);
            // Where the view ray has gone, per unit of depth into the height
            // field, in texture units.
            let reach = -view.xy * flip / max(view.z, 0.2) * depth / a.x;
            let steps = u32(mix(16.0, 6.0, saturate(view.z)));
            var layer = 0.0;
            var previous = 0.0;
            var previous_depth = 1.0;
            for (var i = 0u; i <= steps; i++) {
                layer = f32(i) / f32(steps);
                let surface_depth = 1.0 - ground_set(base_color_map, normal_map, hex, reach * layer, gx, gy).height;
                if surface_depth <= layer {
                    // Between the last step above the surface and this one
                    // below it.
                    let above = previous_depth - previous;
                    let below = layer - surface_depth;
                    layer = mix(previous, layer, above / max(above + below, 1e-4));
                    break;
                }
                previous = layer;
                previous_depth = surface_depth;
            }
            shift = reach * layer;
        }
        var near = ground_set(base_color_map, normal_map, hex, shift, gx, gy);
        if has_transition && breakthrough > 0.0 {
            let t_flip = flip / b.x;
            let other = ground_set(emissive_map, occlusion_map, hex_grid(world * t_flip, globals.floor_d.w), vec2<f32>(0.0), dx.xy * t_flip, dy.xy * t_flip);
            // Height-based blend: the higher of the two surfaces wins, over
            // a band of `PATCH_BAND` in height.
            let ha = near.height + (1.0 - breakthrough);
            let hb = other.height + breakthrough;
            let top = max(ha, hb) - PATCH_BAND;
            let wa = max(ha - top, 0.0);
            let wb = max(hb - top, 0.0);
            near = ground_mix(near, other, wb / max(wa + wb, 1e-4));
        }
        ground = ground_mix(near, far, fade);
    }

    // Variation above the tile, applied after the fade so it stays where
    // the detail has gone to the mean: the ground drifts between the set's
    // two tints (`floors.json`: dry and lush grass, damp and dry earth,
    // stained and worn concrete) over two scales, 5 to 15 m and 30 to
    // 100 m, and mottles over a few metres. None of it is a scaled copy of
    // the maps, which would draw the material's grain at the wrong size.
    // Off for the plain picture the anti-tiling is measured against.
    let measured = b.z > 0.5 && b.z < 1.5;
    let strength = select(a.w, 0.0, measured);
    let pixel = footprint * a.x;
    let broad = field_noise(world, 60.0, 0.0, pixel);
    let local = field_noise(world, 9.0, 1.0, pixel);
    // The noise sum keeps near its middle; the threshold is narrow so that
    // both tints are reached over real areas of ground.
    let zone = smoothstep(0.36, 0.64, 0.6 * broad + 0.4 * local);
    let tint = mix(globals.floor_c.rgb, globals.floor_d.rgb, zone);
    var albedo = ground.albedo * mix(vec3<f32>(1.0), tint, strength);
    // Mottling a few metres across: tufts and bare spots, damp hollows,
    // scuffs.
    let mottle = field_noise(world, 3.5, 4.0, pixel) - 0.5;
    albedo *= 1.0 + 0.6 * strength * mottle;
    let wear = field_noise(world, 21.0, 2.0, pixel) - 0.5;

    var surface: Surface;
    surface.base_color = clamp(albedo, vec3<f32>(0.0), vec3<f32>(0.95));
    surface.metallic = 0.0;
    // The scan's roughness is one blade's or grain's; the ground seen from
    // a few metres is never smoother than its set's least (`floor.rs`).
    let rough = max(ground.roughness, b.w) + strength * 0.2 * wear;
    surface.roughness = toksvig(clamp(rough, 0.05, 1.0), ground.normal_length);
    surface.ao = ground.ao;
    surface.emissive = vec3<f32>(0.0);
    surface.n = normalize(vec3<f32>(-ground.slope, 1.0));
    surface.unevenness = a.z;
    return shade(in, surface, dx, dy);
}
