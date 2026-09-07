use super::*;

const DEFAULT_PALETTE_JSON: &str =
    r##"{"colors":["#ff0080","#00ffc8","#ffbe28","#9d4dff","#3aff8a"]}"##;

const DEFAULT_GRADIENT_JSON: &str =
    r##"{"stops":[{"color":"#000000","t":0},{"color":"#ffffff","t":1}]}"##;

const DEFAULT_CHROMA_PALETTE_JSON: &str = r##"{"colors":["#ff0000","#ff8000","#ffcc00","#ffff00","#80ff00","#00ff00","#00ff80","#00ffff","#0080ff","#0000ff","#8000ff","#ff0080"]}"##;

pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "palette".into(),
            name: "Palette".into(),
            description: Some(
                "Ordered set of K colors emitted as uniformly-spaced color Stops. Use for discrete-feeling color sets (one color per seed, per pitch class, etc.)."
                    .into(),
            ),
            category: Some("Color".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Stops".into(),
                port_type: PortType::Stops,
            }],
            params: vec![ParamDef {
                id: "value".into(),
                name: "Colors".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some(DEFAULT_PALETTE_JSON.into()),
                range: None,
            }],
        },
        NodeTypeDef {
            id: "gradient".into(),
            name: "Gradient".into(),
            description: Some(
                "Continuous color function defined by stops at user-positioned t values. Sampled exactly at consumer time — no fixed-resolution bake."
                    .into(),
            ),
            category: Some("Color".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Stops".into(),
                port_type: PortType::Stops,
            }],
            params: vec![ParamDef {
                id: "value".into(),
                name: "Stops".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some(DEFAULT_GRADIENT_JSON.into()),
                range: None,
            }],
        },
        NodeTypeDef {
            id: "sample_palette".into(),
            name: "Sample Palette".into(),
            description: Some(
                "Samples a Stops function at scalar position u ∈ [0,1]. OKLab interpolation between bracketing stops."
                    .into(),
            ),
            category: Some("Color".into()),
            inputs: vec![
                PortDef {
                    id: "stops".into(),
                    name: "Stops".into(),
                    port_type: PortType::Stops,
                },
                PortDef {
                    id: "u".into(),
                    name: "u".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "chroma_palette".into(),
            name: "Harmonic Palette".into(),
            description: Some(
                "Maps the 12 chroma pitches to colors sampled from a Stops input (12 uniform samples). Falls back to a default rainbow."
                    .into(),
            ),
            category: Some("Color".into()),
            inputs: vec![
                PortDef {
                    id: "chroma".into(),
                    name: "Chroma".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "stops".into(),
                    name: "Stops".into(),
                    port_type: PortType::Stops,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "fallback_palette".into(),
                name: "Fallback Palette".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some(DEFAULT_CHROMA_PALETTE_JSON.into()),
                range: None,
            }],
        },
        NodeTypeDef {
            id: "spectral_shift".into(),
            name: "Spectral Shift".into(),
            description: Some("Rotates color hue based on the dominant musical key.".into()),
            category: Some("Color".into()),
            inputs: vec![
                PortDef {
                    id: "in".into(),
                    name: "Base Color".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "chroma".into(),
                    name: "Chroma".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "strength".into(),
                name: "Strength".into(),
                param_type: ParamType::Number,
                default_number: Some(1.0),
                default_text: None,
                range: None,
            }],
        },
        NodeTypeDef {
            id: "rainbow".into(),
            name: "Rainbow".into(),
            description: Some(
                "Maps a signal through a full rainbow hue cycle. Without input, generates a 256-sample ramp.".into(),
            ),
            category: Some("Color".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Color".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "offset".into(),
                    name: "Offset".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                    range: None,
                },
                ParamDef {
                    id: "spread".into(),
                    name: "Spread".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                    range: None,
                },
                ParamDef {
                    id: "saturation".into(),
                    name: "Saturation".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                    range: None,
                },
            ],
        },
        NodeTypeDef {
            id: "color".into(),
            name: "Color".into(),
            description: Some("Outputs a constant RGB signal.".into()),
            category: Some("Generator".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "color".into(),
                name: "Color".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some(r#"{"r":255,"g":0,"b":0,"a":1}"#.into()),
                range: None,
            }],
        },
    ]
}
