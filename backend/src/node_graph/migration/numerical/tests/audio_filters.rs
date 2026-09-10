use super::*;

#[test]
fn stored_audio_filters_keep_chain_order_cutoffs_and_exposed_sources() {
    let graph: Graph = serde_json::from_value(serde_json::json!({
        "nodes":[
            {"id":"pattern_args","typeId":"pattern_args","params":{}},
            {"id":"high","typeId":"highpass_filter","params":{"cutoff_hz":-20.}},
            {"id":"low","typeId":"lowpass_filter","params":{"cutoff_hz":500.}},
            {"id":"spectrum","typeId":"frequency_amplitude","params":{"selected_frequency_ranges":[[20.,1000.]]}},
            {"id":"out","typeId":"apply_dimmer","params":{}}
        ],
        "edges":[
            {"id":"a","fromNode":"pattern_args","fromPort":"source","toNode":"high","toPort":"audio_in"},
            {"id":"b","fromNode":"high","fromPort":"audio_out","toNode":"low","toPort":"audio_in"},
            {"id":"c","fromNode":"low","fromPort":"audio_out","toNode":"spectrum","toPort":"audio_in"},
            {"id":"d","fromNode":"spectrum","fromPort":"amplitude_out","toNode":"out","toPort":"signal"}
        ],
        "args":[{"id":"source","name":"Follow this stem","argType":"AudioSource","defaultValue":"drums"}]
    })).unwrap();
    let score = crate::node_graph::migration::pattern(&graph, "Filtered drums")
        .unwrap()
        .unwrap();
    let library = score.library(&p::standard_library()).unwrap();
    assert_eq!(
        score.definitions[ROOT].inputs["source"].name,
        "Follow this stem"
    );
    for source in [p::AudioSource::Drums, p::AudioSource::Bass] {
        let program = p::PreparedGraph::new(
            &library,
            ROOT,
            &BTreeMap::from([("source".into(), Value::AudioSource(source.into()))]),
            p::Frame {
                cells: &[],
                features: None,
                beat: 0.,
                clip_start: 0.,
                clip_duration: 4.,
                seed: 0,
            },
        )
        .unwrap();
        let audio = p::AudioInput::from(source)
            .filtered(p::AudioFilter::Highpass { cutoff_hz: 1. })
            .unwrap()
            .filtered(p::AudioFilter::Lowpass { cutoff_hz: 500. })
            .unwrap();
        assert_eq!(
            program.feature_requests(),
            &[p::FeatureRequest::Spectrum {
                source: audio,
                hold_edges: true
            }]
        );
    }
}
