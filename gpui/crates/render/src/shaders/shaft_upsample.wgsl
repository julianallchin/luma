// The sun-shaft fraction (`sun_shafts.wgsl`) read back at full resolution.
// Shared by the scene pass, which scales each fragment's haze by it, and the
// composite, which scales the uncovered sky's.

/// The shaft fraction at `uv`: a tent over the four by four nearest texels,
/// each also weighted by how near its depth is to the pixel's, so a shaft
/// does not bleed across a silhouette. The tent is wider than a bilinear tap
/// because each texel's few samples are noisy; shafts are soft anyway.
///
/// `depth` is the pixel's own view depth; `range` the view depths it covers,
/// which is `depth` twice except for a pixel of open ground toward the
/// horizon, where one pixel spans hundreds of metres.
fn upsample_shafts(fraction: texture_2d<f32>, uv: vec2<f32>, depth: f32, range: vec2<f32>) -> f32 {
    let r = vec2<f32>(textureDimensions(fraction));
    let lr = uv * r - 0.5;
    let base = floor(lr);
    let f = lr - base;
    let tolerance = 0.25 + 0.05 * depth;
    var sum = 0.0;
    var total = 0.0;
    var nearest = 1.0;
    var nearest_err = 1e9;
    for (var j = -1; j < 3; j++) {
        for (var i = -1; i < 3; i++) {
            let texel = clamp(vec2<i32>(base) + vec2<i32>(i, j), vec2<i32>(0), vec2<i32>(r) - 1);
            let s = textureLoad(fraction, texel, 0);
            // Outside the span by the depth tolerance; inside it, by how far
            // in octaves the texel stands from the pixel's own centre.
            let err = max(max(range.x - s.g, s.g - range.y), 0.0) / tolerance
                + 0.25 * abs(log2(max(s.g, 1e-3) / max(depth, 1e-3)));
            let d = abs(vec2<f32>(f32(i), f32(j)) - f);
            let tent = max(2.0 - d.x, 0.0) * max(2.0 - d.y, 0.0);
            let w = tent * exp(-err);
            sum += w * s.r;
            total += w;
            if err < nearest_err {
                nearest = s.r;
                nearest_err = err;
            }
        }
    }
    return select(nearest, sum / total, total > 1e-4);
}
