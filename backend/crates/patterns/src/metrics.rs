//! Geometry and reductions over a resolved fixture domain. Ordering uses an
//! explicit order signal where supplied, with stable identities to break ties.
use crate::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldReduction {
    Minimum,
    Maximum,
    Mean,
    Count,
    DistinctCount,
}

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    use crate::signals::port;
    let (name, inputs, outputs) = match op {
        Primitive::ChannelIndex => (
            "Channel index",
            vec![(
                "value",
                port("Value", ValueType::Signal(SignalType::ANY), None),
            )],
            vec![(
                "value",
                ValueType::Signal(SignalType {
                    unit: Some(Unit::Number),
                    channels: None,
                }),
            )],
        ),
        Primitive::DomainIndex => (
            "Fixture index",
            vec![(
                "value",
                port("Domain", ValueType::Signal(SignalType::ANY), None),
            )],
            vec![("value", ValueType::Number)],
        ),
        Primitive::AlignDomain => (
            "Align fixture domain",
            vec![
                (
                    "value",
                    port("Value", ValueType::Signal(SignalType::ANY), None),
                ),
                (
                    "reference",
                    port("Domain", ValueType::Signal(SignalType::ANY), None),
                ),
                (
                    "order",
                    port(
                        "First fixture order",
                        ValueType::Number,
                        Some(Value::Number(0.)),
                    ),
                ),
            ],
            vec![("value", ValueType::Signal(SignalType::ANY))],
        ),
        Primitive::ChannelCount => (
            "Channel count",
            vec![(
                "value",
                port("Value", ValueType::Signal(SignalType::ANY), None),
            )],
            vec![("value", ValueType::Number)],
        ),
        Primitive::FieldFirst => (
            "First fixture value",
            vec![
                (
                    "value",
                    port("Value", ValueType::Signal(SignalType::ANY), None),
                ),
                (
                    "order",
                    port("Order", ValueType::Number, Some(Value::Number(0.))),
                ),
            ],
            vec![("value", ValueType::Signal(SignalType::ANY))],
        ),
        Primitive::FieldRank => (
            "Rank heads",
            vec![(
                "value",
                port("Value", ValueType::Signal(SignalType::ANY), None),
            )],
            vec![("value", ValueType::Signal(SignalType::ANY))],
        ),
        Primitive::FieldReduce(reduction) => (
            match reduction {
                FieldReduction::Minimum => "Field minimum",
                FieldReduction::Maximum => "Field maximum",
                FieldReduction::Mean => "Field mean",
                FieldReduction::Count => "Head count",
                FieldReduction::DistinctCount => "Distinct values",
            },
            vec![(
                "value",
                port("Value", ValueType::Signal(SignalType::ANY), None),
            )],
            vec![("value", ValueType::Signal(SignalType::ANY))],
        ),
        Primitive::WorldGeometry => (
            "Fixture geometry",
            vec![],
            vec![
                (
                    "position",
                    ValueType::Signal(SignalType::new(
                        Unit::Number,
                        Channels::components(3).unwrap(),
                    )),
                ),
                ("index", ValueType::Number),
            ],
        ),
        Primitive::CirclePhase | Primitive::PrincipalDirection | Primitive::RadialCoordinates => (
            match op {
                Primitive::CirclePhase => "Fit circle",
                Primitive::PrincipalDirection => "Principal direction",
                _ => "Radial coordinates",
            },
            vec![
                (
                    "position",
                    port(
                        "Position",
                        ValueType::Signal(SignalType::new(
                            Unit::Number,
                            Channels::components(if op == Primitive::PrincipalDirection {
                                2
                            } else {
                                3
                            })
                            .unwrap(),
                        )),
                        None,
                    ),
                ),
                (
                    "order",
                    port("Sample order", ValueType::Number, Some(Value::Number(0.))),
                ),
            ],
            if op == Primitive::CirclePhase {
                vec![("phase", ValueType::Number)]
            } else if op == Primitive::RadialCoordinates {
                vec![("phase", ValueType::Number), ("radius", ValueType::Number)]
            } else {
                vec![(
                    "direction",
                    ValueType::Signal(SignalType::new(
                        Unit::Number,
                        Channels::components(2).unwrap(),
                    )),
                )]
            },
        ),
        Primitive::RankNearby => (
            "Rank nearby points",
            vec![
                ("value", port("Sort value", ValueType::Number, None)),
                (
                    "order",
                    port("Tie order", ValueType::Number, Some(Value::Number(0.))),
                ),
                (
                    "position",
                    port(
                        "Position",
                        ValueType::Signal(SignalType::new(
                            Unit::Number,
                            Channels::components(3).unwrap(),
                        )),
                        None,
                    ),
                ),
                (
                    "tolerance",
                    port("Merge distance", ValueType::Number, Some(Value::Number(0.))),
                ),
            ],
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
        inputs: inputs
            .into_iter()
            .map(|(k, mut v)| {
                if op == Primitive::RankNearby && k == "tolerance" {
                    v.rate = Rate::Fixed;
                }
                (k.into(), v)
            })
            .collect(),
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
