use super::*;

#[test]
fn spectrogram_viewers_preserve_audio_bindings_filters_and_historical_grid_connections() {
    let graph: Graph = serde_json::from_value(serde_json::json!({
        "nodes":[
            {"id":"audio","typeId":"audio_input","params":{}},
            {"id":"stems","typeId":"stem_splitter","params":{}},
            {"id":"filter","typeId":"lowpass_filter","params":{"cutoff_hz":350.}},
            {"id":"filtered","typeId":"mel_spec_viewer","params":{}},
            {"id":"unwired","typeId":"mel_spec_viewer","params":{}}
        ],
        "edges":[
            {"id":"stems","fromNode":"audio","fromPort":"out","toNode":"stems","toPort":"audio_in"},
            {"id":"filter","fromNode":"stems","fromPort":"drums_out","toNode":"filter","toPort":"audio_in"},
            {"id":"view","fromNode":"filter","fromPort":"audio_out","toNode":"filtered","toPort":"in"},
            {"id":"grid","fromNode":"audio","fromPort":"grid_out","toNode":"filtered","toPort":"grid"}
        ], "args":[]
    })).unwrap();
    // Diagnostic-only authored graphs need no synthetic fixture capability.
    let score = crate::node_graph::migration::pattern(&graph, "Spectrogram")
        .unwrap()
        .unwrap();
    let score: p::Score = serde_json::from_value(serde_json::to_value(score).unwrap()).unwrap();
    let prepared = p::PreparedGraph::new(
        &score.library(&p::standard_library()).unwrap(),
        ROOT,
        &BTreeMap::new(),
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
    assert!(
        prepared.feature_requests().is_empty(),
        "an inspection source loaded audio into the playback graph"
    );
    let source = p::AudioInput::from(p::AudioSource::Drums)
        .filtered(p::AudioFilter::Lowpass { cutoff_hz: 350. })
        .unwrap();
    for beat in [2., 0., 2.] {
        let values = prepared.evaluate(beat).unwrap();
        assert_eq!(values["view/filtered"], Value::AudioSource(source.clone()));
        assert_eq!(
            values["view/unwired"],
            Value::AudioSource(p::AudioSource::Mix.into())
        );
        assert!(values["lighting"].validate().is_ok());
    }
    assert_eq!(
        prepared.evaluate_batch(&[0.]).unwrap()["lighting"]
            .lighting()
            .unwrap()
            .writes(),
        [false; 5]
    );
    let mut invalid = graph.clone();
    invalid.edges[3].from_port = "out".into();
    assert!(convert(&invalid, "Invalid grid")
        .unwrap_err()
        .contains("needs the track beat grid"));
    let mut invalid = graph;
    invalid.nodes.push(
        serde_json::from_value(
            serde_json::json!({"id":"number","typeId":"scalar","params":{"value":1.}}),
        )
        .unwrap(),
    );
    invalid.edges[2].from_node = "number".into();
    invalid.edges[2].from_port = "out".into();
    assert!(convert(&invalid, "Invalid audio")
        .unwrap_err()
        .contains("needs audio"));
}

#[test]
fn event_viewers_preserve_event_data_instead_of_baking_it_into_preview_bins() {
    let graph: Graph = serde_json::from_value(serde_json::json!({
        "nodes":[
            {"id":"beat","typeId":"beat_pulses","params":{}},
            {"id":"drums","typeId":"drum_events","params":{}},
            {"id":"beat_view","typeId":"view_events","params":{}},
            {"id":"snare_view","typeId":"view_events","params":{}},
            {"id":"level","typeId":"scalar","params":{"value":0.5}},
            {"id":"out","typeId":"apply_dimmer","params":{}}
        ],
        "edges":[
            {"id":"beat_view","fromNode":"beat","fromPort":"events_out","toNode":"beat_view","toPort":"events_in"},
            {"id":"snare_view","fromNode":"drums","fromPort":"snare_out","toNode":"snare_view","toPort":"events_in"},
            {"id":"out","fromNode":"level","fromPort":"out","toNode":"out","toPort":"signal"}
        ], "args":[]
    })).unwrap();
    let score = crate::node_graph::migration::pattern(&graph, "Event inspection")
        .unwrap()
        .unwrap();
    let score: p::Score = serde_json::from_value(serde_json::to_value(score).unwrap()).unwrap();
    for id in ["beat_view", "snare_view"] {
        assert_eq!(
            score.definitions[ROOT].outputs[&format!("view/{id}")].value_type,
            ValueType::Events
        );
    }
    let grid = BeatGrid {
        beats: vec![0., 0.5, 1.],
        downbeats: vec![0.],
        bpm: 120.,
        beats_per_bar: 4,
        downbeat_offset: 0.,
    };
    let clock = grid.timeline().unwrap();
    let features = std::sync::Arc::new(ReferenceFeatures {
        clock: clock.clone(),
        timing: std::sync::Arc::new(grid.timing().unwrap()),
        onsets: BTreeMap::from([("snare".into(), vec![0.1, 0.11, 0.4])]),
        harmony: vec![],
        audio: BTreeMap::new(),
    });
    let prepared = p::PreparedGraph::new(
        &score.library(&p::standard_library()).unwrap(),
        ROOT,
        &BTreeMap::new(),
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
    assert_eq!(
        prepared
            .feature_requests()
            .iter()
            .filter_map(|request| match request {
                p::FeatureRequest::Onsets(drum) => Some(*drum),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![p::Drum::Snare]
    );
    let prepared = prepared.with_features(features).unwrap();
    for times in [&[1., 0., 1.][..], &[0.25][..]] {
        let result = prepared.evaluate_batch(times).unwrap();
        for (id, seconds) in [
            ("beat_view", vec![0., 0.5, 1.]),
            ("snare_view", vec![0.1_f32, 0.4]),
        ] {
            let expected = Value::Events(p::Events::Beats {
                times: p::EventTimes::new(
                    seconds
                        .into_iter()
                        .map(|s| clock.beat_at(f64::from(s)).unwrap())
                        .collect::<Vec<_>>(),
                )
                .unwrap(),
            });
            for i in 0..times.len() {
                assert_eq!(result[&format!("view/{id}")].sample(i).unwrap(), expected);
            }
        }
    }
    let mut invalid = graph.clone();
    invalid.edges[0].from_port = "typo".into();
    assert!(convert(&invalid, "Invalid event port").is_err());
    let mut invalid = graph;
    invalid.edges[0].from_node = "level".into();
    invalid.edges[0].from_port = "out".into();
    assert!(convert(&invalid, "Invalid event source")
        .unwrap_err()
        .contains("needs events"));
}
