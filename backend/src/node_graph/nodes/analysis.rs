use super::*;
pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![
        NodeTypeDef {
            id: "mel_spec_viewer".into(),
            name: "Mel Spectrogram".into(),
            description: Some("Shows the mel spectrogram for the chosen track.".into()),
            category: Some("View".into()),
            inputs: vec![
                PortDef {
                    id: "in".into(),
                    name: "Audio".into(),
                    port_type: PortType::Audio,
                },
                PortDef {
                    id: "grid".into(),
                    name: "Beat Grid".into(),
                    port_type: PortType::BeatGrid,
                },
            ],
            outputs: vec![],
            params: vec![],
        },
        NodeTypeDef {
            id: "harmony_analysis".into(),
            name: "Harmony Analysis".into(),
            description: Some(
                "Detects chords from incoming audio and exposes a confidence timeline.".into(),
            ),
            category: Some("Audio".into()),
            inputs: vec![PortDef {
                id: "audio_in".into(),
                name: "Audio".into(),
                port_type: PortType::Audio,
            }],
            outputs: vec![PortDef {
                id: "signal".into(),
                name: "Chroma (Signal)".into(),
                port_type: PortType::Signal,
            }],
            params: vec![],
        },
        NodeTypeDef {
            id: "view_signal".into(),
            name: "View Signal".into(),
            description: Some("Displays the incoming signal (flattened to 1D preview).".into()),
            category: Some("View".into()),
            inputs: vec![PortDef {
                id: "in".into(),
                name: "Signal".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![],
            params: vec![],
        },
        NodeTypeDef {
            id: "view_events".into(),
            name: "View Events".into(),
            description: Some(
                "Displays an event stream as discrete pulses on the simulation grid.".into(),
            ),
            category: Some("View".into()),
            inputs: vec![PortDef {
                id: "events_in".into(),
                name: "Events".into(),
                port_type: PortType::Events,
            }],
            outputs: vec![],
            params: vec![],
        },
        NodeTypeDef {
            id: "view_uv".into(),
            name: "View UV".into(),
            description: Some(
                "Displays UV plane spot positions for each selected spotlight at the current playhead."
                    .into(),
            ),
            category: Some("View".into()),
            inputs: vec![PortDef {
                id: "uv".into(),
                name: "UV".into(),
                port_type: PortType::Signal,
            }],
            outputs: vec![],
            params: vec![],
        },
    ]
}
