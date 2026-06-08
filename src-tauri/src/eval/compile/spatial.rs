//! Lowering for spatial nodes. Owns: get_attribute, mirror.
//!
//! `get_attribute { attribute: "rel_x" | "pos_y" | "angular_index" | ... }` maps
//! the attribute string to a `SpatialOp` variant (`Pos(Axis)`, `Rel(Axis)`,
//! `Index`, `NormalizedIndex`, `AngularIndex`, `AngularPosition`, `CircleRadius`,
//! `Mirror(Axis)`, ...). `mirror { axis }` -> `SpatialOp::Mirror`. These are
//! prologue (t-invariant), output `n = plan.n`, `c = 1`. The `selection` input
//! port just scopes which primitives — for v1 it's the whole selection (plan.n).
//!
//! NOTE: spatial ops read `ResidentContext.positions`; the golden runner must
//! supply real positions for these patterns to match (else they read zeros).
//! Reference: legacy `node_graph/nodes/selection.rs` + `eval/ops/spatial.rs`.

use super::{CompileError, LowerCtx, Lowerer};
use crate::eval::ops::spatial::{Axis, SpatialOp};
use crate::eval::{OpKind, Phase};

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
            let attr = lc.param_str("attribute").unwrap_or_else(|| "index".to_string());
            let op = attr_to_op(&attr);
            low.emit(OpKind::Spatial(op), vec![], n, 1, Phase::Prologue, id, "out");
            Some(Ok(()))
        }
        "mirror" => {
            let axis = lc.param_str("axis").unwrap_or_else(|| "x".to_string());
            let op = SpatialOp::Mirror(axis_of(&axis));
            low.emit(OpKind::Spatial(op), vec![], n, 1, Phase::Prologue, id, "out");
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
            params: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
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
        let lc = LowerCtx { node, edges: &edges, args: &args, by_id: &by_id };
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
                OpKind::Spatial(op) => assert_eq!(
                    format!("{op:?}"),
                    format!("{expected:?}"),
                    "attr {attr}"
                ),
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

    #[test]
    fn mirror_maps_axis_param() {
        let nd = node("m", "mirror", &[("axis", Value::from("z"))]);
        match lower_one(&nd, 4) {
            OpKind::Spatial(SpatialOp::Mirror(Axis::Z)) => {}
            other => panic!("expected Mirror(Z), got {other:?}"),
        }
        // Default axis is X when param absent.
        let nd2 = node("m2", "mirror", &[]);
        match lower_one(&nd2, 4) {
            OpKind::Spatial(SpatialOp::Mirror(Axis::X)) => {}
            other => panic!("expected Mirror(X), got {other:?}"),
        }
    }
}
