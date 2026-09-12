// Stub for devices without subgroup ballots: the deterministic compute
// kernel integrates exactly as `beam_scatter` does and touches no cache.
fn beam_scatter_cached(li: u32, ray: SceneRay, sigma: f32) -> vec3<f32> {
    return beam_scatter(li, ray, sigma);
}
fn cache_locate(pixel: vec2<u32>, lane: u32) {}
// Quad-sharing needs the ballot and shuffles too; inert without subgroups.
override QUADSHARE: u32 = 1u;
override QUAD_UNIFORM: u32 = 1u;
override QUAD_APEX_PX: f32 = 32.0;
override QUAD_GATE_M: f32 = 0.25;
override QUAD_GATE_FRAC: f32 = 0.01;
override QUAD_SPAN_MEAN: u32 = 1u;
override QUAD_SPLIT_FETCH: u32 = 1u;
override QUAD_DIAG: u32 = 0u;
override QUAD_INTERP: u32 = 1u;
fn quad_prepare(frag: vec2<f32>, ray: SceneRay) {}
