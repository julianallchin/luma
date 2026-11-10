// A face of cells behind one cone (`panel.rs`): the colour its light has at
// a point, from the cells that point faces.
//
// The consumer declares `panel_data`, the packed panels: for each, a header
// of two words (the face's right and half its width; half its height, its
// columns, its rows and one cell's optic radius), then the summed-area table
// of its cells, one entry wider and taller than the grid.

/// The table's sum of the cells left of and below `p`, in cells. The cells
/// are constant across themselves, so their sum is linear between entries.
fn panel_sum(table: u32, grid: vec2<f32>, p: vec2<f32>) -> vec3<f32> {
    let cell = min(floor(p), grid - 1.0);
    let f = p - cell;
    let stride = u32(grid.x) + 1u;
    let o = table + u32(cell.y) * stride + u32(cell.x);
    return mix(
        mix(panel_data[o].rgb, panel_data[o + 1u].rgb, f.x),
        mix(panel_data[o + stride].rgb, panel_data[o + stride + 1u].rgb, f.x),
        f.y,
    );
}

/// The colour a cone casts toward the point `q` metres from its lens centre.
/// `panel` is where its cells start in `panel_data`, or zero for a cone with
/// none, whose colour is `color` everywhere.
///
/// The point is lit by the cells round its own place on the face: the
/// nearest point of the face to it. How far round is its beam's radius at
/// its distance from that place, since a cell's light spreads that far by
/// the time it arrives. Once that radius spans the face the answer is
/// `color`, the cells' mean, and the table is not read.
///
/// `smear` widens the radius, metres: a ray through the haze asks once for
/// the whole stretch of it that passes the face, not for one point of it.
fn panel_colour(panel: f32, color: vec3<f32>, direction: vec3<f32>, cos_beam: f32, q: vec3<f32>, smear: f32) -> vec3<f32> {
    if panel < 0.5 {
        return color;
    }
    let start = u32(panel);
    let head = panel_data[start];
    let shape = panel_data[start + 1u];
    let half = vec2<f32>(head.w, shape.x);
    let up = cross(direction, head.xyz);
    let at = clamp(vec2<f32>(dot(q, head.xyz), dot(q, up)), -half, half);
    let cos = clamp(cos_beam, 1e-3, 1.0);
    let radius = shape.w + smear + length(q - head.xyz * at.x - up * at.y) * sqrt(1.0 - cos * cos) / cos;
    if radius >= 2.0 * max(half.x, half.y) {
        return color;
    }
    let grid = shape.yz;
    let lo = clamp((at - radius + half) / (2.0 * half), vec2<f32>(0.0), vec2<f32>(1.0)) * grid;
    let hi = clamp((at + radius + half) / (2.0 * half), vec2<f32>(0.0), vec2<f32>(1.0)) * grid;
    let table = start + 2u;
    let sum = panel_sum(table, grid, hi) - panel_sum(table, grid, vec2<f32>(lo.x, hi.y))
        - panel_sum(table, grid, vec2<f32>(hi.x, lo.y)) + panel_sum(table, grid, lo);
    return sum / ((hi.x - lo.x) * (hi.y - lo.y));
}
