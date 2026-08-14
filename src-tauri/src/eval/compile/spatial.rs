//! Lowering for spatial nodes. Owns: get_attribute, mirror.
//!
//! `get_attribute { attribute: "rel_x" | "pos_y" | "angular_index" | ... }` maps
//! the attribute string to a `SpatialOp` variant (`Pos(Axis)`, `Rel(Axis)`,
//! `U`, `V`, `Index`, `NormalizedIndex`, `AngularIndex`, `AngularPosition`,
//! `CircleRadius`, `Mirror(Axis)`, ...). `attr_to_op` below is the authoritative
//! attribute list; the node def (`node_graph/nodes/selection.rs`), the UI
//! dropdown (`src/shared/lib/react-flow/get-attribute-node.tsx`) and the docs
//! (`www/content/docs/architecture/selection-system.mdx`) mirror it by hand and
//! have to be updated alongside it.
//! `mirror { axis }` -> `SpatialOp::Mirror`. These are
//! prologue (t-invariant), output `n = plan.n`, `c = 1`. The `selection` input
//! port just scopes which primitives — for v1 it's the whole selection (plan.n).
//!
//! NOTE: spatial ops read `ResidentContext.positions`; the golden runner must
//! supply real positions for these patterns to match (else they read zeros).
//! Reference: legacy `node_graph/nodes/selection.rs` + `eval/ops/spatial.rs`.

use super::{CompileError, LowerCtx, Lowerer};
use crate::eval::ops::spatial::{Axis, SpatialOp};
use crate::eval::{OpKind, Phase};
use crate::models::node_graph::Stops;
use crate::node_graph::oklab::srgb_to_oklab;

/// Chroma-rescue boost cap (ported from legacy `MAX_CHROMA_BOOST`).
const MAX_CHROMA_BOOST: f32 = 10.0;

/// Convert an sRGB stop to `(L, a, b, alpha)` in OKLab.
fn stop_to_lab(rgba: [f32; 4]) -> [f32; 4] {
    let (l, a, b) = srgb_to_oklab(rgba[0], rgba[1], rgba[2]);
    [l, a, b, rgba[3]]
}

fn chroma_rescue(a: &mut f32, b: &mut f32, c_now: f32, c_target: f32, vibrance: f32) {
    let c_final = c_now + (c_target - c_now) * vibrance;
    if c_now > 1e-6 {
        let scale = (c_final / c_now).min(MAX_CHROMA_BOOST).max(0.0);
        *a *= scale;
        *b *= scale;
    }
}

/// Sample a `Stops` function at `u ∈ [0,1]` in OKLab with the vibrance chroma
/// rescue (so interpolated seeds don't collapse to grey at complementary-color
/// midpoints). Ported 1:1 from the legacy `sample_stops_lab`.
fn sample_stops_lab(stops: &Stops, u: f32, vibrance: f32) -> [f32; 4] {
    if stops.stops.is_empty() {
        return [0.0, 0.0, 0.0, 1.0];
    }
    if stops.stops.len() == 1 {
        return stop_to_lab(stops.stops[0].1);
    }
    let u = u.clamp(0.0, 1.0);
    if u <= stops.stops[0].0 {
        return stop_to_lab(stops.stops[0].1);
    }
    let last = stops.stops.len() - 1;
    if u >= stops.stops[last].0 {
        return stop_to_lab(stops.stops[last].1);
    }
    let (mut lo, mut hi) = (0usize, last);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if stops.stops[mid].0 <= u {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (t0, c_lo) = stops.stops[lo];
    let (t1, c_hi) = stops.stops[hi];
    let frac = ((u - t0) / (t1 - t0).max(1e-6)).clamp(0.0, 1.0);
    let lo = stop_to_lab(c_lo);
    let hi = stop_to_lab(c_hi);
    let c_lo_mag = (lo[1] * lo[1] + lo[2] * lo[2]).sqrt();
    let c_hi_mag = (hi[1] * hi[1] + hi[2] * hi[2]).sqrt();
    let l = lo[0] + (hi[0] - lo[0]) * frac;
    let mut a = lo[1] + (hi[1] - lo[1]) * frac;
    let mut b = lo[2] + (hi[2] - lo[2]) * frac;
    let c_now = (a * a + b * b).sqrt();
    let c_target = c_lo_mag + (c_hi_mag - c_lo_mag) * frac;
    chroma_rescue(&mut a, &mut b, c_now, c_target, vibrance);
    let alpha = lo[3] + (hi[3] - lo[3]) * frac;
    [l, a, b, alpha]
}

/// Bake the `K` per-seed OKLab colors + their chroma magnitudes from the palette.
/// `K == stops.len()` uses the stops 1:1; otherwise samples `K` evenly-spaced
/// positions (matching the legacy node).
fn bake_palette(stops: &Stops, k: usize, vibrance: f32) -> (Vec<[f32; 4]>, Vec<f32>) {
    let lab: Vec<[f32; 4]> = if k == stops.stops.len() {
        stops
            .stops
            .iter()
            .map(|(_, rgba)| stop_to_lab(*rgba))
            .collect()
    } else {
        (0..k)
            .map(|i| {
                let u = if k == 1 {
                    0.0
                } else {
                    i as f32 / (k - 1) as f32
                };
                sample_stops_lab(stops, u, vibrance)
            })
            .collect()
    };
    let chroma = lab
        .iter()
        .map(|c| (c[1] * c[1] + c[2] * c[2]).sqrt())
        .collect();
    (lab, chroma)
}

/// Map an attribute string to its `SpatialOp` variant. Unknown strings fall back
/// to the stubbed `GetAttribute(attr)` (returns zeros). Mirrors the legacy
/// `get_attribute` attribute table in `node_graph/nodes/selection.rs`.
fn attr_to_op(attr: &str) -> SpatialOp {
    match attr {
        "pos_x" | "x" => SpatialOp::Pos(Axis::X),
        "pos_y" | "y" => SpatialOp::Pos(Axis::Y),
        "pos_z" | "z" => SpatialOp::Pos(Axis::Z),
        "rel_x" => SpatialOp::Rel(Axis::X),
        "rel_y" => SpatialOp::Rel(Axis::Y),
        "rel_z" => SpatialOp::Rel(Axis::Z),
        "u" => SpatialOp::U,
        "v" => SpatialOp::V,
        "rel_major_span" => SpatialOp::RelMajorSpan,
        "rel_major_count" => SpatialOp::RelMajorCount,
        "index" => SpatialOp::Index,
        "normalized_index" => SpatialOp::NormalizedIndex,
        "angular_index" => SpatialOp::AngularIndex,
        "angular_position" => SpatialOp::AngularPosition,
        "circle_radius" => SpatialOp::CircleRadius,
        "mirror_x" => SpatialOp::Mirror(Axis::X),
        "mirror_y" => SpatialOp::Mirror(Axis::Y),
        "mirror_z" => SpatialOp::Mirror(Axis::Z),
        other => SpatialOp::GetAttribute(other.to_string()),
    }
}

/// `axis` param string ("x"/"y"/"z") -> `Axis`, defaulting to X (legacy default).
fn axis_of(axis: &str) -> Axis {
    match axis {
        "y" => Axis::Y,
        "z" => Axis::Z,
        _ => Axis::X,
    }
}

pub fn lower_spatial(lc: &LowerCtx, low: &mut Lowerer) -> Option<Result<(), CompileError>> {
    let id = &lc.node.id;
    let n = low.n;
    match lc.type_id() {
        "get_attribute" => {
            let attr = lc
                .param_str("attribute")
                .unwrap_or_else(|| "index".to_string());
            let op = attr_to_op(&attr);
            // If the `selection` input carries a c=3 position slot (a `mirror`
            // fold upstream), feed it as a position override so the attribute is
            // computed on the mirrored geometry.
            let inputs = match lc.input(low, "selection") {
                Some(s) if low.slot_shape(s).1 == 3 => vec![s],
                _ => vec![],
            };
            low.emit(
                OpKind::Spatial(op),
                inputs,
                n,
                1,
                Phase::Prologue,
                id,
                lc.out_port(),
            );
            Some(Ok(()))
        }
        "mirror" => {
            let axis = axis_of(&lc.param_str("axis").unwrap_or_else(|| "x".to_string()));
            // `out` = folded positions (a c=3 slot consumed downstream as a
            // position override); `side` = the +1/-1/0 side scalar.
            let folded = low.emit(
                OpKind::Spatial(SpatialOp::Fold(axis)),
                vec![],
                n,
                3,
                Phase::Prologue,
                id,
                "out",
            );
            // c=3 here is world position, not the usual RGB triple.
            low.label(folded, &["x", "y", "z"]);
            low.emit(
                OpKind::Spatial(SpatialOp::Mirror(axis)),
                vec![],
                n,
                1,
                Phase::Prologue,
                id,
                "side",
            );
            Some(Ok(()))
        }
        "soft_voronoi" => {
            let stops = match lc.resolve_stops("stops") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let num_points = lc.const_input("num_points", 6.0).clamp(1.0, 64.0).round() as usize;
            let softness = lc.const_input("softness", 0.3);
            let vibrance = lc.const_input("vibrance", 0.6);
            let wander_speed = lc.const_input("wander_speed", 0.3);
            let seed_offset = lc.param_f32("seed_offset", 0.0) as u64;
            let seed = lc.seed() ^ seed_offset;
            let (lab_palette, lab_chroma) = bake_palette(&stops, num_points, vibrance);
            // Time-varying RGBA color field (c=4) → feeds apply_color like any
            // other color source.
            low.emit(
                OpKind::Spatial(SpatialOp::SoftVoronoi {
                    num_points,
                    softness,
                    vibrance,
                    wander_speed,
                    seed,
                    lab_palette,
                    lab_chroma,
                }),
                vec![],
                n,
                4,
                Phase::Kernel,
                id,
                lc.out_port(),
            );
            Some(Ok(()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::OpKind;
    use crate::models::node_graph::NodeInstance;
    use serde_json::Value;
    use std::collections::HashMap;

    fn node(id: &str, type_id: &str, params: &[(&str, Value)]) -> NodeInstance {
        NodeInstance {
            id: id.into(),
            type_id: type_id.into(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            position_x: None,
            position_y: None,
        }
    }

    /// Lower a single-node graph and return the emitted op's kind.
    fn lower_one(node: &NodeInstance, n: u32) -> OpKind {
        let edges = Vec::new();
        let args = HashMap::new();
        let by_id: HashMap<&str, &NodeInstance> =
            std::iter::once((node.id.as_str(), node)).collect();
        let lc = LowerCtx {
            node,
            edges: &edges,
            args: &args,
            by_id: &by_id,
            grid: None,
            onsets: None,
        };
        let mut low = Lowerer::new(n);
        let r = lower_spatial(&lc, &mut low).expect("claimed").expect("ok");
        let _ = r;
        assert_eq!(low.ops.len(), 1, "exactly one op emitted");
        let op = &low.ops[0];
        assert_eq!(low.slots[op.out as usize].n, n);
        assert_eq!(low.slots[op.out as usize].c, 1);
        assert!(matches!(op.phase, Phase::Prologue));
        op.kind.clone()
    }

    #[test]
    fn get_attribute_maps_to_variants() {
        let cases = [
            ("rel_x", SpatialOp::Rel(Axis::X)),
            ("pos_z", SpatialOp::Pos(Axis::Z)),
            ("x", SpatialOp::Pos(Axis::X)),
            ("y", SpatialOp::Pos(Axis::Y)),
            ("u", SpatialOp::U),
            ("v", SpatialOp::V),
            ("normalized_index", SpatialOp::NormalizedIndex),
            ("angular_index", SpatialOp::AngularIndex),
            ("angular_position", SpatialOp::AngularPosition),
            ("circle_radius", SpatialOp::CircleRadius),
            ("index", SpatialOp::Index),
            ("mirror_y", SpatialOp::Mirror(Axis::Y)),
        ];
        for (attr, expected) in cases {
            let nd = node("ga", "get_attribute", &[("attribute", Value::from(attr))]);
            let kind = lower_one(&nd, 7);
            match kind {
                OpKind::Spatial(op) => {
                    assert_eq!(format!("{op:?}"), format!("{expected:?}"), "attr {attr}")
                }
                other => panic!("expected Spatial, got {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_attribute_falls_back_to_get_attribute_stub() {
        let nd = node("ga", "get_attribute", &[("attribute", Value::from("gobo"))]);
        match lower_one(&nd, 3) {
            OpKind::Spatial(SpatialOp::GetAttribute(s)) => assert_eq!(s, "gobo"),
            other => panic!("expected GetAttribute stub, got {other:?}"),
        }
    }

    /// Lower a single node, returning all emitted op kinds (in order).
    fn lower_all(node: &NodeInstance, n: u32) -> Vec<OpKind> {
        let edges = Vec::new();
        let args = HashMap::new();
        let by_id: HashMap<&str, &NodeInstance> =
            std::iter::once((node.id.as_str(), node)).collect();
        let lc = LowerCtx {
            node,
            edges: &edges,
            args: &args,
            by_id: &by_id,
            grid: None,
            onsets: None,
        };
        let mut low = Lowerer::new(n);
        lower_spatial(&lc, &mut low).expect("claimed").expect("ok");
        low.ops.iter().map(|o| o.kind.clone()).collect()
    }

    #[test]
    fn mirror_emits_fold_and_side() {
        // `mirror` emits the folded-positions op (`out`) + the side scalar (`side`).
        let ops = lower_all(&node("m", "mirror", &[("axis", Value::from("z"))]), 4);
        let kinds: Vec<String> = ops.iter().map(|k| format!("{k:?}")).collect();
        assert!(
            kinds.iter().any(|k| k.contains("Fold(Z)")),
            "fold: {kinds:?}"
        );
        assert!(
            kinds.iter().any(|k| k.contains("Mirror(Z)")),
            "side: {kinds:?}"
        );
        // Default axis is X when param absent.
        let ops2 = lower_all(&node("m2", "mirror", &[]), 4);
        let kinds2: Vec<String> = ops2.iter().map(|k| format!("{k:?}")).collect();
        assert!(
            kinds2.iter().any(|k| k.contains("Fold(X)")),
            "fold: {kinds2:?}"
        );
    }
}
