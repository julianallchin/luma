use luma_patterns::*;
use std::collections::BTreeMap;

fn frame() -> Frame<'static> {
    Frame {
        cells: &[],
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 4.,
        seed: 0,
    }
}
fn wire(node: &str) -> Binding {
    Binding::Connection {
        node: node.into(),
        output: "source".into(),
    }
}

#[test]
fn filter_chains_prepare_one_immutable_source_and_keep_original_wire_encoding() {
    let mut library = standard_library();
    let mut definition = library.definitions["audio_spectrum"].instance("audio_spectrum");
    let Body::Graph(graph) = &mut definition.body else {
        panic!()
    };
    let spectrum = graph.nodes.values_mut().next().unwrap();
    spectrum.inputs.insert("source".into(), wire("low"));
    graph.nodes.insert(
        "high".into(),
        Node {
            definition: "audio_highpass".into(),
            position: None,
            inputs: BTreeMap::from([
                (
                    "source".into(),
                    Binding::Input {
                        input: "source".into(),
                    },
                ),
                ("cutoff_hz".into(), Value::Number(80.).into()),
            ]),
        },
    );
    graph.nodes.insert(
        "low".into(),
        Node {
            definition: "audio_lowpass".into(),
            position: None,
            inputs: BTreeMap::from([
                ("source".into(), wire("high")),
                ("cutoff_hz".into(), Value::Number(500.).into()),
            ]),
        },
    );
    library.definitions.insert("filtered".into(), definition);
    library.validate("filtered").unwrap();
    let source: AudioInput = AudioSource::Drums.into();
    let value = Value::AudioSource(source.clone());
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        r#"{"type":"audio_source","value":"drums"}"#
    );
    let expected = source
        .filtered(AudioFilter::Highpass { cutoff_hz: 80. })
        .unwrap()
        .filtered(AudioFilter::Lowpass { cutoff_hz: 500. })
        .unwrap();
    let roundtrip: Value = serde_json::from_str(
        &serde_json::to_string(&Value::AudioSource(expected.clone())).unwrap(),
    )
    .unwrap();
    assert_eq!(roundtrip, Value::AudioSource(expected.clone()));
    let graph = PreparedGraph::new(
        &library,
        "filtered",
        &BTreeMap::from([("source".into(), value)]),
        frame(),
    )
    .unwrap();
    assert_eq!(
        graph.feature_requests(),
        &[FeatureRequest::Spectrum {
            source: expected,
            hold_edges: false
        }]
    );
    // Building the filtered source itself needs no track features or playback.
    let filter = PreparedGraph::new(&library, "audio_lowpass", &BTreeMap::new(), frame()).unwrap();
    assert!(filter.feature_requests().is_empty());
    assert_eq!(
        filter.evaluate(0.).unwrap(),
        filter.evaluate(1000.).unwrap()
    );
}

#[test]
fn invalid_audio_chains_fail_before_track_preparation() {
    let source: AudioInput = AudioSource::Mix.into();
    for cutoff_hz in [f64::NAN, f64::INFINITY, -1., 0.] {
        assert!(source.filtered(AudioFilter::Lowpass { cutoff_hz }).is_err());
    }
    let mut long = source;
    for _ in 0..64 {
        long = long
            .filtered(AudioFilter::Lowpass { cutoff_hz: 200. })
            .unwrap();
    }
    assert!(long
        .filtered(AudioFilter::Highpass { cutoff_hz: 300. })
        .is_err());
    assert!(serde_json::from_str::<Value>(
        r#"{"type":"audio_source","value":{"source":"bass","filters":[],"typo":1}}"#
    )
    .is_err());
    assert!(PreparedGraph::new(
        &standard_library(),
        "audio_lowpass",
        &BTreeMap::from([("cutoff_hz".into(), Value::Number(-1.))]),
        frame()
    )
    .is_err());
}
