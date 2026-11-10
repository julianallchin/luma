// A face of cells as one light (`panel.rs`): the light its cells send to a
// point. The lit cells' rectangle, evenly lit, under the beam's true kernel,
// from a table of the kernel's integral; times the pattern inside it, the
// face's texture over the same rectangle evenly lit, both under one Gaussian
// blur as wide as the beam is there.
//
// The consumer declares `panel_data`, the packed panels, and prepends
// `fixture_light.wgsl` for the beam's profile. Each panel is a header of
// twelve words (the face's right and one texel's side; the texture's middle,
// its side in texels and the field's edge as an angle; the beam's weight, the
// blur's width and half the lit cells' extent; the lit cells' centre and
// spread; the same for each quarter of them; each quarter's light, in its
// colour and in white), then the kernel's table, then the pattern and its
// blurred levels.

// Words in a panel's header, and cells along each side of its kernel table.
const PANEL_HEAD: u32 = 12u;
const PANEL_KERNEL: u32 = 32u;
// Samples of margin on each side of a blurred level (`MARGIN` in `panel.rs`).
const PANEL_MARGIN: i32 = 3;

struct PanelLight {
    // rgb: what `colour · profile / distance²` is for a cone. a: the same for
    // a white face, for the part of a beam's spectrum that is not its colour.
    light: vec4<f32>,
    // Where on the face the light comes from, from the cone's position.
    source: vec3<f32>,
}

/// One sample of a level `n` samples a side that starts at `table`. Past the
/// level's edge the pattern is as it was at the edge.
fn panel_texel(table: i32, n: i32, x: i32, y: i32) -> vec4<f32> {
    return panel_data[u32(table + clamp(y, 0, n - 1) * n + clamp(x, 0, n - 1))];
}

/// The stack's `level` at `at` metres from the texture's middle, between its
/// samples. Level zero is the texture; the blurred levels come in pairs that
/// share a grid, the first pair the texture's own and each later pair one
/// twice as coarse.
fn panel_level(stack: u32, side: i32, texel: f32, level: u32, at: vec2<f32>) -> vec4<f32> {
    var table = i32(stack);
    var n = side;
    var grid = texel;
    if level > 0u {
        table += side * side;
        n = side + 2 * PANEL_MARGIN;
        for (var pair = 0u; pair < (level - 1u) / 2u; pair += 1u) {
            table += 2 * n * n;
            n = max(side >> (pair + 1u), 2) + 2 * PANEL_MARGIN;
            grid *= 2.0;
        }
        if (level - 1u) % 2u == 1u {
            table += n * n;
        }
    }
    let p = at / grid + 0.5 * f32(n - 1);
    let cell = floor(p);
    let f = p - cell;
    let x = i32(cell.x);
    let y = i32(cell.y);
    return mix(
        mix(panel_texel(table, n, x, y), panel_texel(table, n, x + 1, y), f.x),
        mix(panel_texel(table, n, x, y + 1), panel_texel(table, n, x + 1, y + 1), f.x),
        f.y,
    );
}

/// What a quarter of the lit cells, `lit` (their centre and their spread),
/// sends to the point `at` across the face and `height` in front of it, as
/// one cone: the beam's profile from their centre, widened by their spread.
fn panel_cone(lit: vec4<f32>, at: vec2<f32>, height: f32, blur2: f32, cos_beam: f32, cos_field: f32) -> f32 {
    let share = vec2<f32>(blur2) / (vec2<f32>(blur2) + lit.zw);
    let t = (at - lit.xy) * sqrt(share) / height;
    let cos2 = 1.0 / (1.0 + dot(t, t));
    return sqrt(share.x * share.y) * angular_profile(sqrt(cos2), cos_beam, cos_field) * cos2 / (height * height);
}

/// The kernel's integral over the rectangle from the axis to `(u, v)`,
/// tangents as a point sees it, negative where that crosses one axis. `table`
/// holds the integral at the four corners of each cell, in even steps of
/// angle out to `edge`.
fn panel_kernel(table: u32, edge: f32, u: f32, v: f32) -> f32 {
    let p = min(atan(abs(vec2<f32>(u, v))) / edge, vec2<f32>(1.0)) * f32(PANEL_KERNEL);
    let cell = min(floor(p), vec2<f32>(f32(PANEL_KERNEL) - 1.0));
    let f = p - cell;
    let c = panel_data[table + u32(cell.y) * PANEL_KERNEL + u32(cell.x)];
    return sign(u) * sign(v) * mix(mix(c.x, c.y, f.x), mix(c.z, c.w, f.x), f.y);
}

/// The light the panel at `panel` in `panel_data` sends to the point `q`
/// metres from its cone's position.
///
/// Close to the face it is the lit cells' rectangle under the beam's kernel,
/// times the pattern the texture makes inside it. Once the beam is a few
/// times wider than the lit cells themselves it is the light of each quarter
/// of them from its own centre, through the beam's profile widened by its
/// spread: one cone, far off.
fn panel_light(panel: f32, direction: vec3<f32>, cos_beam: f32, cos_field: f32, q: vec3<f32>) -> PanelLight {
    let start = u32(panel);
    let head = panel_data[start];
    let shape = panel_data[start + 1u];
    let beam = panel_data[start + 2u];
    let lit = panel_data[start + 3u];
    var out: PanelLight;
    out.light = vec4<f32>(0.0);
    out.source = vec3<f32>(0.0);
    let height = dot(q, direction);
    if height <= 0.0 {
        return out;
    }
    let up = cross(direction, head.xyz);
    let at = vec2<f32>(dot(q, head.xyz), dot(q, up));
    let blur = beam.y * height;
    let blur2 = blur * blur;
    // How much of the light's spread at this distance is the beam's and not
    // the cells' own: the light comes from that much nearer the cells'
    // centre than the point is.
    let share = vec2<f32>(blur2) / (vec2<f32>(blur2) + lit.zw);
    let seat = lit.xy + (at - lit.xy) * (vec2<f32>(1.0) - share);
    out.source = head.xyz * seat.x + up * seat.y;
    let far = smoothstep(2.0, 4.0, blur / sqrt(max(lit.z, lit.w)));
    if far > 0.0 {
        // One cone a quarter, from that quarter's own centre.
        for (var quarter = 0u; quarter < 4u; quarter += 1u) {
            let light = panel_data[start + 8u + quarter];
            if light.a > 0.0 {
                out.light += light * (far * panel_cone(panel_data[start + 4u + quarter], at, height, blur2, cos_beam, cos_field));
            }
        }
    }
    if far < 1.0 {
        // The blur and the texture's own grain together, in half texels,
        // squared: one for the texture, and doubling a level. The two levels
        // that bracket this blur are mixed to give a lone cell the
        // brightness it leaves: brightness goes as one over the square.
        let reach2 = 4.0 * blur2 / (head.w * head.w) + 1.0;
        // Three pairs of levels more than the texture has halvings.
        let side = i32(shape.z);
        let last = 2u * (firstTrailingBit(u32(side)) + 3u) - 1u;
        let level = min(u32(max(floor(log2(reach2)), 0.0)), last);
        let mixing = clamp(2.0 * (1.0 - exp2(f32(level)) / reach2), 0.0, 1.0);
        // The pattern, read inside the lit rectangle: past its edge it is
        // as it was at the edge.
        let inside = clamp(at, -beam.zw, beam.zw) - shape.xy;
        let stack = start + PANEL_HEAD + PANEL_KERNEL * PANEL_KERNEL;
        let pattern = mix(
            panel_level(stack, side, head.w, level, inside),
            panel_level(stack, side, head.w, level + 1u, inside),
            mixing,
        );
        // The rectangle, evenly lit, under the beam's own kernel.
        let table = start + PANEL_HEAD;
        let lo = (-beam.zw - at) / height;
        let hi = (beam.zw - at) / height;
        let rectangle = (panel_kernel(table, shape.w, hi.x, hi.y) - panel_kernel(table, shape.w, lo.x, hi.y)
            - panel_kernel(table, shape.w, hi.x, lo.y) + panel_kernel(table, shape.w, lo.x, lo.y))
            / (4.0 * beam.z * beam.w);
        out.light += pattern * (rectangle * (1.0 - far));
    }
    return out;
}

/// How finely a ray must be sampled between the points `qa` and `qb` (from
/// the cone's position) to resolve the panel's cells: one piece per cell it
/// passes, or per blur's width where the cells have run together, and one
/// where the face is already one cone.
fn panel_pieces(panel: f32, direction: vec3<f32>, qa: vec3<f32>, qb: vec3<f32>) -> u32 {
    let start = u32(panel);
    let head = panel_data[start];
    let shape = panel_data[start + 1u];
    let beam = panel_data[start + 2u];
    let lit = panel_data[start + 3u];
    let nearest = max(min(dot(qa, direction), dot(qb, direction)), 0.0);
    let spread = sqrt(max(lit.z, lit.w));
    let blur = beam.y * nearest;
    if blur >= 4.0 * spread {
        return 1u;
    }
    let along = qb - qa;
    let up = cross(direction, head.xyz);
    let across = length(vec2<f32>(dot(along, head.xyz), dot(along, up)));
    // Only the stretch that passes the face has cells to resolve.
    let passing = min(across, shape.z * head.w + 8.0 * spread);
    return clamp(u32(ceil(passing / max(2.0 * head.w, 2.0 * blur))), 1u, 32u);
}
