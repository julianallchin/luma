//! Geometry and reductions over a resolved head domain. Ordering always uses
//! stable head identities to break ties, never fixture or graph traversal order.
use crate::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldReduction {
    Minimum,
    Maximum,
    Mean,
    Count,
}

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    use crate::signals::port;
    let (name, inputs, outputs) = match op {
        Primitive::FieldRank => (
            "Rank heads",
            vec![("value", port("Value", ValueType::Field, None))],
            vec![("value", ValueType::Field)],
        ),
        Primitive::FieldReduce(reduction) => (
            match reduction {
                FieldReduction::Minimum => "Field minimum",
                FieldReduction::Maximum => "Field maximum",
                FieldReduction::Mean => "Field mean",
                FieldReduction::Count => "Head count",
            },
            vec![("value", port("Value", ValueType::Field, None))],
            vec![("value", ValueType::Number)],
        ),
        Primitive::StageCoordinates => (
            "Stage coordinates (meters)",
            vec![],
            vec![
                ("u", ValueType::Field),
                ("v", ValueType::Field),
                ("z", ValueType::Field),
            ],
        ),
        _ => return None,
    };
    Some(Definition {
        name: name.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs: outputs
            .into_iter()
            .map(|(k, value_type)| {
                (
                    k.into(),
                    Output {
                        value_type,
                        rate: Rate::Frame,
                    },
                )
            })
            .collect(),
        body: Body::Primitive(op),
    })
}

pub(crate) fn run(
    op: Primitive,
    inputs: &BTreeMap<String, Value>,
    frame: Frame,
) -> Option<Result<BTreeMap<String, Value>>> {
    if op == Primitive::StageCoordinates {
        return Some(Ok(["u", "v", "z"]
            .into_iter()
            .enumerate()
            .map(|(axis, key)| {
                (
                    key.into(),
                    Value::Field(
                        frame
                            .cells
                            .iter()
                            .map(|c| (c.id.clone(), c.uvz[axis]))
                            .collect(),
                    ),
                )
            })
            .collect()));
    }
    if !matches!(op, Primitive::FieldRank | Primitive::FieldReduce(_)) {
        return None;
    }
    let Value::Field(field) = &inputs["value"] else {
        unreachable!("validated field")
    };
    let value = match op {
        Primitive::FieldRank => {
            let mut ordered: Vec<_> = field.iter().collect();
            ordered.sort_by(|(a_id, a), (b_id, b)| {
                // Treat -0 and +0 as the same value; the identity breaks ties.
                if a == b {
                    a_id.cmp(b_id)
                } else {
                    a.total_cmp(b)
                }
            });
            Value::Field(
                ordered
                    .into_iter()
                    .enumerate()
                    .map(|(rank, (id, _))| (id.clone(), rank as f64))
                    .collect(),
            )
        }
        Primitive::FieldReduce(reduction) => Value::Number(if field.is_empty() {
            0.0
        } else {
            match reduction {
                FieldReduction::Count => field.len() as f64,
                FieldReduction::Minimum => field.values().copied().fold(f64::INFINITY, f64::min),
                FieldReduction::Maximum => {
                    field.values().copied().fold(f64::NEG_INFINITY, f64::max)
                }
                // Divide first to avoid overflowing a sum of otherwise valid values.
                FieldReduction::Mean => field.values().map(|v| v / field.len() as f64).sum(),
            }
        }),
        _ => unreachable!(),
    };
    Some(
        value
            .validate()
            .map(|_| BTreeMap::from([("value".into(), value)])),
    )
}
