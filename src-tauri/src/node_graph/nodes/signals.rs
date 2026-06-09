use super::*;
pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "math".into(),
            name: "Math".into(),
            description: Some("Performs math operations on signals with broadcasting.".into()),
            category: Some("Transform".into()),
            inputs: vec![
                PortDef {
                    id: "a".into(),
                    name: "A".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "b".into(),
                    name: "B".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "operation".into(),
                name: "Operation".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("add".into()), // add, subtract, multiply, divide, max, min, abs_diff, abs, modulo, circular_distance
            }],
        },
        NodeTypeDef {
            id: "round".into(),
            name: "Round".into(),
            description: Some("Quantizes signal values (floor, ceil, round).".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "operation".into(),
                name: "Operation".into(),
                param_type: ParamType::Text,
                default_number: None,
                default_text: Some("round".into()), // round, floor, ceil
            }],
        },
        NodeTypeDef {
            id: "ramp".into(),
            name: "Time Ramp".into(),
            description: Some(
                "Generates a linear ramp from 0 to n_beats over the pattern duration.".into(),
            ),
            category: Some("Generator".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "ramp_between".into(),
            name: "Linear Ramp".into(),
            description: Some(
                "Generates a linear ramp from start to end signals over the pattern duration."
                    .into(),
            ),
            category: Some("Generator".into()),
            inputs: vec![
                PortDef {
                    id: "start".into(),
                    name: "Start".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "end".into(),
                    name: "End".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "threshold".into(),
            name: "Threshold".into(),
            description: Some("Binarizes a signal using a cutoff value.".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "threshold".into(),
                name: "Threshold".into(),
                param_type: ParamType::Number,
                default_number: Some(0.5),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "normalize".into(),
            name: "Normalize (0-1)".into(),
            description: Some(
                "Normalizes an input signal into the 0..1 range using min/max over the whole time series."
                    .into(),
            ),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "falloff".into(),
            name: "Falloff".into(),
            description: Some("Applies a soft falloff to a normalized signal (0..1).".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "width".into(),
                    name: "Width".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "curve".into(),
                    name: "Curve".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "invert".into(),
            name: "Invert".into(),
            description: Some("Reflects a signal around its observed midpoint.".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "sine_wave".into(),
            name: "Sine Wave".into(),
            description: Some("Generates a beat-synced sine wave. Subdivision controls cycles per beat (1 = one full cycle per beat, 0.5 = every 2 beats, 2 = twice per beat).".into()),
            category: Some("Generator".into()),
            inputs: vec![PortDef {
                id: "subdivision".into(),
                name: "Subdivision".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "subdivision".into(),
                    name: "Subdivision".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "phase_deg".into(),
                    name: "Phase (deg)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "amplitude".into(),
                    name: "Amplitude".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "offset".into(),
                    name: "Offset".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "noise".into(),
            name: "Noise".into(),
            description: Some(
                "Generates 3D fractal noise. Samples at (x, y, time) coordinates."
                    .into(),
            ),
            category: Some("Generator".into()),
            inputs: vec![
                PortDef {
                    id: "time".into(),
                    name: "Time".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "x".into(),
                    name: "X".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "y".into(),
                    name: "Y".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "scale".into(),
                    name: "Scale".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "octaves".into(),
                    name: "Octaves".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "amplitude".into(),
                    name: "Amplitude".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "offset".into(),
                    name: "Offset".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "scale".into(),
                    name: "Scale".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "octaves".into(),
                    name: "Octaves".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "amplitude".into(),
                    name: "Amplitude".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "offset".into(),
                    name: "Offset".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "remap".into(),
            name: "Remap".into(),
            description: Some("Linearly maps an input range [in_min..in_max] to [out_min..out_max].".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "in_min".into(),
                    name: "In Min".into(),
                    param_type: ParamType::Number,
                    default_number: Some(-1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "in_max".into(),
                    name: "In Max".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "out_min".into(),
                    name: "Out Min".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "out_max".into(),
                    name: "Out Max".into(),
                    param_type: ParamType::Number,
                    default_number: Some(180.0),
                    default_text: None,
                },
                ParamDef {
                    id: "clamp".into(),
                    name: "Clamp".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
            ],
        },
        // ----- Movement perturbation nodes -----
        NodeTypeDef {
            id: "circle".into(),
            name: "Circle".into(),
            description: Some(
                "Circular motion in UV space. Outputs normalized (u,v) in [-1,1].".into(),
            ),
            category: Some("Movement".into()),
            inputs: vec![PortDef {
                id: "phase".into(),
                name: "Phase Offset".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "uv".into(),
                name: "UV".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "radius".into(),
                    name: "Radius".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "speed".into(),
                    name: "Speed (cycles/beat)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.25),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "figure_8".into(),
            name: "Figure 8".into(),
            description: Some(
                "Lissajous 2:1 figure-eight motion in UV space. Outputs normalized (u,v) in [-1,1]."
                    .into(),
            ),
            category: Some("Movement".into()),
            inputs: vec![PortDef {
                id: "phase".into(),
                name: "Phase Offset".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "uv".into(),
                name: "UV".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "width".into(),
                    name: "Width".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "height".into(),
                    name: "Height".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.5),
                    default_text: None,
                },
                ParamDef {
                    id: "speed".into(),
                    name: "Speed (cycles/beat)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.25),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "sweep".into(),
            name: "Sweep".into(),
            description: Some(
                "Linear sweep at an angle in UV space. 0\u{00b0}=U axis, 90\u{00b0}=V axis. Outputs normalized (u,v) in [-1,1]."
                    .into(),
            ),
            category: Some("Movement".into()),
            inputs: vec![PortDef {
                id: "phase".into(),
                name: "Phase Offset".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "uv".into(),
                name: "UV".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "angle".into(),
                    name: "Angle (deg)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.0),
                    default_text: None,
                },
                ParamDef {
                    id: "range".into(),
                    name: "Range".into(),
                    param_type: ParamType::Number,
                    default_number: Some(1.0),
                    default_text: None,
                },
                ParamDef {
                    id: "speed".into(),
                    name: "Speed (cycles/beat)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.5),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "wander".into(),
            name: "Wander".into(),
            description: Some(
                "Noise-based organic drift in UV space. Outputs normalized (u,v) in [-1,1]."
                    .into(),
            ),
            category: Some("Movement".into()),
            inputs: vec![PortDef {
                id: "phase".into(),
                name: "Phase Offset".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "uv".into(),
                name: "UV".into(),
                port_type: PortType::Signal,
            }],
            params: vec![
                ParamDef {
                    id: "radius".into(),
                    name: "Radius".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.5),
                    default_text: None,
                },
                ParamDef {
                    id: "speed".into(),
                    name: "Speed (cycles/beat)".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.25),
                    default_text: None,
                },
                ParamDef {
                    id: "smoothness".into(),
                    name: "Smoothness".into(),
                    param_type: ParamType::Number,
                    default_number: Some(2.0),
                    default_text: None,
                },
            ],
        },
        NodeTypeDef {
            id: "scalar".into(),
            name: "Scalar".into(),
            description: Some("Outputs a constant scalar value.".into()),
            category: Some("Generator".into()),
            inputs: vec![],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "value".into(),
                name: "Value".into(),
                param_type: ParamType::Number,
                default_number: Some(1.0),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "modulo".into(),
            name: "Modulo".into(),
            description: Some("Wraps input values to range [0, divisor). Useful for looping animations.".into()),
            category: Some("Transform".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![ParamDef {
                id: "divisor".into(),
                name: "Divisor".into(),
                param_type: ParamType::Number,
                default_number: Some(1.0),
                default_text: None,
            }],
        },
        NodeTypeDef {
            id: "time_delay".into(),
            name: "Time Delay".into(),
            description: Some(
                "Delays a signal in time per-fixture. Positive delay = lag, negative = advance."
                    .into(),
            ),
            category: Some("Transform".into()),
            inputs: vec![
                PortDef {
                    id: "in".into(),
                    name: "Signal".into(),
                    port_type: PortType::Signal,
                },
                PortDef {
                    id: "delay".into(),
                    name: "Delay (s)".into(),
                    port_type: PortType::Signal,
                },
            ],
            outputs: vec![PortDef {
                id: "out".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
    ]
}
