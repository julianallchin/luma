// Stub for devices without subgroup ballots: the deterministic compute
// kernel integrates exactly as `beam_scatter` does and touches no cache.
fn beam_scatter_cached(li: u32, ray: SceneRay, sigma: f32) -> vec3<f32> {
    return beam_scatter(li, ray, sigma);
}
fn cache_locate(pixel: vec2<u32>, lane: u32) {}
fn quad_prepare(frag: vec2<f32>, ray: SceneRay) {}
