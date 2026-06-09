use super::*;
pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![NodeTypeDef {
        id: "soft_voronoi".into(),
        name: "Soft Voronoi".into(),
        description: Some(
            "K wandering seed points within the selection's bounding volume, blended in OKLab to a per-fixture color. Softness controls the softmin temperature (fraction of bbox diagonal). Vibrance lerps the blended OKLab chroma magnitude toward the weight-averaged input chroma so blends don't go muddy."
                .into(),
        ),
        category: Some("Spatial".into()),
        inputs: vec![
            PortDef {
                id: "selection".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            },
            PortDef {
                id: "stops".into(),
                name: "Stops".into(),
                port_type: PortType::Stops,
            },
            PortDef {
                id: "num_points".into(),
                name: "Points".into(),
                port_type: PortType::Signal,
            },
            PortDef {
                id: "softness".into(),
                name: "Softness".into(),
                port_type: PortType::Signal,
            },
            PortDef {
                id: "vibrance".into(),
                name: "Vibrance".into(),
                port_type: PortType::Signal,
            },
            PortDef {
                id: "wander_speed".into(),
                name: "Wander Speed".into(),
                port_type: PortType::Signal,
            },
        ],
        outputs: vec![PortDef {
            id: "out".into(),
            name: "Color".into(),
            port_type: PortType::Signal,
        }],
        params: vec![
            ParamDef {
                id: "num_points".into(),
                name: "Points".into(),
                param_type: ParamType::Number,
                default_number: Some(6.0),
                default_text: None,
            },
            ParamDef {
                id: "wander_speed".into(),
                name: "Wander Speed".into(),
                param_type: ParamType::Number,
                default_number: Some(0.3),
                default_text: None,
            },
            ParamDef {
                id: "softness".into(),
                name: "Softness".into(),
                param_type: ParamType::Number,
                default_number: Some(0.3),
                default_text: None,
            },
            ParamDef {
                id: "vibrance".into(),
                name: "Vibrance".into(),
                param_type: ParamType::Number,
                default_number: Some(0.6),
                default_text: None,
            },
            ParamDef {
                id: "seed_offset".into(),
                name: "Seed Offset".into(),
                param_type: ParamType::Number,
                default_number: Some(0.0),
                default_text: None,
            },
        ],
    }]
}
