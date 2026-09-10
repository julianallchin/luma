// Indoor distance fade shared by opaque surfaces and transparent geometry.
// Apply it before blending: a foreground cable must never inherit the depth
// of the floor behind it. The composite fills the uncovered fraction with
// the background after all scene geometry has accumulated its coverage.
const HORIZON_NEAR_M: f32 = 100.0;
const HORIZON_FAR_M: f32 = 700.0;
const HORIZON_BIAS: f32 = 1.5;

fn horizon_coverage(view_depth: f32) -> f32 {
    // Outdoors, finite atmospheric transport replaces the artistic dissolve.
    if aerial_sky.sun.w > 0.5 { return 1.0; }
    let debug = u32(globals.params.w + 0.5);
    if debug >= 1u && debug <= 5u {
        return 1.0;
    }
    // Inverse depth spreads the ground's dissolve across screen rows even
    // at a grazing angle. The floor extends beyond the far end of this band.
    let span = 1.0 - HORIZON_NEAR_M / HORIZON_FAR_M;
    let t = saturate((1.0 - HORIZON_NEAR_M / max(view_depth, 1e-3)) / span);
    return 1.0 - pow(t, HORIZON_BIAS);
}
