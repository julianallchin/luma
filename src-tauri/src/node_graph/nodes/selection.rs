use super::*;
pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "get_attribute".into(),
            name: "Get Attribute".into(),
            description: Some("Extracts spatial attributes from a selection into a Signal.".into()),
            category: Some("Selection".into()),
            inputs: vec![PortDef {
                id: "selection".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "attribute".into(),
                name: "Attribute".into(),
                param_type: ParamType::enum_of(crate::eval::ops::spatial::ATTRIBUTES),
                default_number: None,
                default_text: Some("index".into()),
            }],
        },
        NodeTypeDef {
            id: "random_select_mask".into(),
            name: "Random Select Mask".into(),
            description: Some(
                "Re-rolls a random subset of N items on every incoming event.".into(),
            ),
            category: Some("Selection".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "events_in".into(),
                    name: "Events".into(),
                    port_type: PortType::Events,
                },
                PortDef {
                    id: "count".into(),
                    name: "Count".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Mask".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "avoid_repeat".into(),
                name: "Avoid Repeat".into(),
                param_type: ParamType::Number, // 0 or 1
                default_number: Some(1.0),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "filter_selection".into(),
            name: "Filter Selection".into(),
            description: Some(
                "Filters a selection to only include fixtures with a specific capability.".into(),
            ),
            category: Some("Selection".into()),
            inputs: vec![PortDef {
                id: "selection".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            params: vec![ParamDef {
                id: "capability".into(),
                name: "Capability".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("movement".into()), // movement, color, strobe
            }],
        },
        NodeTypeDef {
            id: "mirror".into(),
            name: "Mirror".into(),
            description: Some(
                "Folds fixture positions across a mirror axis for symmetric spatial effects."
                    .into(),
            ),
            category: Some("Selection".into()),
            inputs: vec![PortDef {
                id: "selection".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            }],
            outputs: vec![
                PortDef {
                    id: "out".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "side".into(),
                    name: "Side".into(),
                    port_type: PortType::Signal,
                },
            ],
            params: vec![ParamDef {
                id: "axis".into(),
                name: "Axis".into(),
                param_type: ParamType::enum_of(crate::eval::ops::spatial::AXES),
                default_number: None,
                default_text: Some("x".into()),
            }],
        },
    ]
}
