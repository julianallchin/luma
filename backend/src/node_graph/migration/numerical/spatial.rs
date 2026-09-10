//! Historical spatial cards become ordinary geometry/arithmetic subgraphs.
use super::*;

impl Builder {
    fn reduce(&mut self, op: &str, value: B) -> B {
        self.call(&format!("core/{op}"), [("value", value)], "value")
    }
    fn exact_greater(&mut self, a: B, b: B) -> B {
        self.call(
            "core/greater",
            [("a", a), ("b", b), ("tolerance", number(0.))],
            "mask",
        )
    }
    fn normalized(&mut self, value: B) -> B {
        let min = self.reduce("field_minimum", value.clone());
        let max = self.reduce("field_maximum", value.clone());
        let range = self.math("subtract", max, min.clone());
        let range = self.math("maximum", range, number(1e-3));
        let delta = self.math("subtract", value, min);
        self.math("divide", delta, range)
    }
    fn centered(&mut self, value: B) -> B {
        let center = self.reduce("field_mean", value.clone());
        self.math("subtract", value, center)
    }
    fn mirror_center(&mut self, value: B) -> B {
        let center = self.reduce("field_mean", value);
        let abs = self.unary("absolute", center.clone());
        let snapped = self.exact_greater(number(0.1), abs);
        self.choose(snapped, number(0.), center)
    }
    fn mirror_side(&mut self, value: B) -> B {
        let center = self.mirror_center(value.clone());
        let upper = self.math("add", center.clone(), number(0.5));
        let lower = self.math("subtract", center, number(0.5));
        let positive = self.exact_greater(value.clone(), upper);
        let negative = self.exact_greater(lower, value);
        self.math("subtract", positive, negative)
    }
    fn rig_u(&mut self, x: B, y: B, index: B) -> B {
        let ranges: Vec<_> = [x.clone(), y.clone()]
            .into_iter()
            .map(|value| {
                let lo = self.reduce("field_minimum", value.clone());
                let hi = self.reduce("field_maximum", value);
                self.math("subtract", hi, lo)
            })
            .collect();
        let extent = self.math("maximum", ranges[0].clone(), ranges[1].clone());
        let degenerate = self.exact_greater(number(1e-3), extent);
        let xy = self.join(x.clone(), y.clone());
        let direction = self.call(
            "core/principal_direction",
            [("position", xy), ("order", index.clone())],
            "direction",
        );
        let ax = self.channel_at(direction.clone(), 0);
        let ay = self.channel_at(direction, 1);
        let dx = self.centered(x);
        let dy = self.centered(y);
        let count = self.reduce("head_count", dx.clone());
        let px = self.math("multiply", dx, ax);
        let py = self.math("multiply", dy, ay);
        let position = self.math("add", px, py);
        let position = self.normalized(position);
        let denominator = self.math("subtract", count, number(1.));
        let fallback = self.math("divide", index, denominator);
        self.choose(degenerate, fallback, position)
    }
}

pub(super) fn lower(
    node: &NodeInstance,
    widths: &BTreeMap<String, Option<usize>>,
) -> Result<Definition, String> {
    let mut b = Builder::default();
    let geometry = b.node("fixture_geometry", []);
    let position = if node.type_id == "get_attribute" && widths.get("selection") == Some(&Some(3)) {
        b.input(
            "selection",
            "Positions",
            ValueType::Signal(p::SignalType::new(
                p::Unit::Number,
                p::Channels::components(3).unwrap(),
            )),
            None,
        )
    } else {
        wire(&geometry, "position")
    };
    let xyz: [B; 3] = std::array::from_fn(|i| b.channel_at(position.clone(), i));
    let axis_index = |name: &str| match name {
        "x" => Ok(0),
        "y" => Ok(1),
        "z" => Ok(2),
        _ => Err(format!("unknown mirror axis {name}")),
    };
    if node.type_id == "mirror" {
        let axis = axis_index(
            node.params
                .get("axis")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("x"),
        )?;
        let value = xyz[axis].clone();
        let center = b.mirror_center(value.clone());
        let offset = b.math("subtract", value.clone(), center);
        let folded = b.unary("absolute", offset);
        let side = b.mirror_side(value);
        let mut xyz = xyz;
        xyz[axis] = folded;
        let xy = b.join(xyz[0].clone(), xyz[1].clone());
        let out = b.join(xy, xyz[2].clone());
        return b.finish("Mirror positions", [("out", out), ("side", side)]);
    }
    let attr = node
        .params
        .get("attribute")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("index");
    let index = wire(&geometry, "index");
    let out = match attr {
        "index" => index,
        "count" => b.reduce("head_count", index),
        "normalized_index" => {
            let count = b.reduce("head_count", index.clone());
            let denominator = b.math("subtract", count, number(1.));
            b.math("divide", index, denominator)
        }
        "x" | "y" | "z" | "pos_x" | "pos_y" | "pos_z" => {
            xyz[axis_index(attr.trim_start_matches("pos_"))?].clone()
        }
        "rel_x" | "rel_y" | "rel_z" => b.normalized(xyz[axis_index(&attr[4..])?].clone()),
        "mirror_x" | "mirror_y" | "mirror_z" => b.mirror_side(xyz[axis_index(&attr[7..])?].clone()),
        "rel_major_span" | "rel_major_count" => {
            let mut metrics = Vec::new();
            for value in &xyz {
                let metric = if attr == "rel_major_count" {
                    let mm = b.math("multiply", value.clone(), number(1000.));
                    let mm = b.round(mm);
                    let mm = b.math("minimum", mm, number(i32::MAX as f64));
                    let mm = b.math("maximum", mm, number(i32::MIN as f64));
                    b.reduce("distinct_count", mm)
                } else {
                    let min = b.reduce("field_minimum", value.clone());
                    let max = b.reduce("field_maximum", value.clone());
                    let range = b.math("subtract", max, min);
                    b.math("maximum", range, number(1e-3))
                };
                metrics.push(metric);
            }
            let y_over_x = b.exact_greater(metrics[1].clone(), metrics[0].clone());
            let xy_metric = b.math("maximum", metrics[0].clone(), metrics[1].clone());
            let z_over_xy = b.exact_greater(metrics[2].clone(), xy_metric);
            let x = b.normalized(xyz[0].clone());
            let y = b.normalized(xyz[1].clone());
            let z = b.normalized(xyz[2].clone());
            let xy = b.choose(y_over_x, y, x);
            b.choose(z_over_xy, z, xy)
        }
        "u" => b.rig_u(xyz[0].clone(), xyz[1].clone(), index),
        "v" => {
            let min = b.reduce("field_minimum", xyz[2].clone());
            let max = b.reduce("field_maximum", xyz[2].clone());
            let range = b.math("subtract", max, min);
            let degenerate = b.exact_greater(number(1e-3), range);
            let normalized = b.normalized(xyz[2].clone());
            b.choose(degenerate, number(0.5), normalized)
        }
        "circle_radius" => b.call(
            "core/radial_coordinates",
            [("position", position), ("order", index)],
            "radius",
        ),
        "angular_index" => {
            let phase = b.call(
                "core/radial_coordinates",
                [("position", position.clone()), ("order", index.clone())],
                "phase",
            );
            b.call(
                "core/rank_nearby",
                [
                    ("value", phase),
                    ("position", position),
                    ("tolerance", number(0.5)),
                    ("order", index),
                ],
                "value",
            )
        }
        "angular_position" => b.call(
            "core/fit_circle",
            [("position", position), ("order", index)],
            "phase",
        ),
        _ => return Err(format!("unknown spatial attribute {attr}")),
    };
    b.finish(&format!("Attribute: {attr}"), [("out", out)])
}
