use super::*;
pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        // `apply_dimmer` is not offered in the node palette; brightness comes
        // from Apply Color. Saved graphs that use it load through the graph
        // migration.
        NodeTypeDef {
            id: "apply_color".into(),
            name: "Apply Color".into(),
            description: Some("Applies RGB(A) signal to selected primitives.".into()),
            category: Some("Output".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "signal".into(),
                    name: "Signal (4ch)".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![], // No output wire, contributes to Layer
            params: vec![],
        },
        NodeTypeDef {
            id: "apply_strobe".into(),
            name: "Apply Strobe".into(),
            description: Some("Applies a strobe signal to selected primitives.".into()),
            category: Some("Output".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "signal".into(),
                    name: "Signal (1ch)".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![], // No output wire, contributes to Layer
            params: vec![],
        },
        // `apply_position` is not offered in the node palette; use Apply
        // Movement. Saved graphs that use it load through the graph migration.
        NodeTypeDef {
            id: "apply_movement".into(),
            name: "Apply Movement".into(),
            description: Some(
                "Maps UV perturbation through the group's movement pyramid to absolute pan/tilt per fixture."
                    .into(),
            ),
            category: Some("Output".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "uv".into(),
                    name: "UV (2ch)".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![],
            params: vec![],
        },
        NodeTypeDef {
            id: "apply_speed".into(),
            name: "Apply Speed".into(),
            description: Some(
                "Applies movement speed to selected primitives. 0 = frozen, 1 = fast (binary)."
                    .into(),
            ),
            category: Some("Output".into()),
            inputs: vec![
                PortDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    port_type: PortType::Selection,
                },
                PortDef {
                    id: "speed".into(),
                    name: "Speed".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![],
            params: vec![],
        },
    ]
}
