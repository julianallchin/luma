use luma_patterns::*;
use ndarray::Array3;
use std::{collections::BTreeMap, sync::Arc};

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

#[derive(Debug)]
struct Spectra {
    fault: Option<&'static str>,
}
impl FeatureSource for Spectra {
    fn onsets(&self, _: Drum) -> Result<EventTimes> {
        Err(Error("no onsets".into()))
    }
    fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<FeatureSample> {
        assert_eq!(
            request,
            &FeatureRequest::Spectrum {
                source: AudioSource::Vocals.into(),
                hold_edges: false
            }
        );
        let mut bins = vec![beat.abs(), 0.5, 2.];
        let mut bin_hz = 20.;
        match self.fault {
            Some("width") if beat > 0. => {
                bins.pop();
            }
            Some("spacing") if beat > 0. => bin_hz = 21.,
            Some("negative") => bins[0] = -1.,
            Some("infinite") => bins[0] = f64::INFINITY,
            Some("empty") => bins.clear(),
            Some("zero spacing") => bin_hz = 0.,
            Some("wrong") => return Ok(FeatureSample::Energy(1.)),
            _ => (),
        }
        Ok(FeatureSample::Spectrum { bins, bin_hz })
    }
}

#[test]
fn spectra_are_channel_vectors_and_seek_without_playback_history() {
    let library = standard_library();
    let graph = PreparedGraph::new(
        &library,
        "audio_spectrum",
        &BTreeMap::from([(
            "source".into(),
            Value::AudioSource(AudioSource::Vocals.into()),
        )]),
        frame(),
    )
    .unwrap();
    assert_eq!(
        graph.feature_requests(),
        &[FeatureRequest::Spectrum {
            source: AudioSource::Vocals.into(),
            hold_edges: false
        }]
    );
    assert!(graph.evaluate(0.).is_err());
    let graph = graph
        .with_features(Arc::new(Spectra { fault: None }))
        .unwrap();
    let output = graph.evaluate_batch(&[1., 3., 1.]).unwrap();
    let spectrum = output["spectrum"].signal().unwrap();
    assert_eq!(spectrum.values().dim(), (1, 3, 3));
    assert_eq!(spectrum.fixtures(), None);
    assert_eq!(spectrum.unit(), Unit::Number);
    assert_eq!(*spectrum.channels(), Channels::components(3).unwrap());
    assert_eq!(
        spectrum.values().iter().copied().collect::<Vec<_>>(),
        vec![1., 0.5, 2., 3., 0.5, 2., 1., 0.5, 2.]
    );
    assert_eq!(output["bin_hz"].signal().unwrap().values()[[0, 0, 0]], 20.);
    assert_eq!(
        graph.evaluate_batch(&[]).unwrap()["spectrum"]
            .signal()
            .unwrap()
            .values()
            .dim(),
        (1, 0, 3)
    );
    assert_eq!(
        graph.evaluate(1.).unwrap()["spectrum"],
        output["spectrum"].sample(2).unwrap()
    );
}

#[test]
fn malformed_spectra_and_changing_frequency_axes_are_rejected() {
    for fault in [
        "width",
        "spacing",
        "negative",
        "infinite",
        "empty",
        "zero spacing",
        "wrong",
    ] {
        let graph = PreparedGraph::new(
            &standard_library(),
            "audio_spectrum",
            &BTreeMap::from([(
                "source".into(),
                Value::AudioSource(AudioSource::Vocals.into()),
            )]),
            frame(),
        )
        .unwrap();
        let result = graph
            .with_features(Arc::new(Spectra { fault: Some(fault) }))
            .and_then(|g| g.evaluate_batch(&[0., 1.]));
        assert!(result.is_err(), "{fault}");
    }
}

#[test]
fn channel_index_and_sum_preserve_fixture_time_axes_and_sum_units() {
    let values = Signal::new(
        Array3::from_shape_vec(
            (2, 2, 3),
            vec![1., 2., 3., 4., 5., 6., 7., 8., 9., 10., 11., 12.],
        )
        .unwrap(),
        Unit::Degrees,
        Channels::components(3).unwrap(),
        Some(vec!["z".into(), "a".into()].into()),
    )
    .unwrap();
    let library = standard_library();
    let inputs = BTreeMap::from([("value".into(), Value::Signal(values))]);
    let sum = PreparedGraph::new(&library, "core/channel_sum", &inputs, frame())
        .unwrap()
        .evaluate_batch(&[0., 1.])
        .unwrap();
    let sum = sum["value"].signal().unwrap();
    assert_eq!(sum.unit(), Unit::Degrees);
    assert_eq!(sum.fixtures().unwrap(), &["a", "z"]);
    assert_eq!(sum.values().dim(), (2, 2, 1));
    assert_eq!(
        sum.values().iter().copied().collect::<Vec<_>>(),
        vec![24., 33., 6., 15.]
    );
    let index = PreparedGraph::new(&library, "core/channel_index", &inputs, frame())
        .unwrap()
        .evaluate_batch(&[0., 1.])
        .unwrap();
    let index = index["value"].signal().unwrap();
    assert_eq!(index.unit(), Unit::Number);
    assert_eq!(index.fixtures(), sum.fixtures());
    assert_eq!(index.values().dim(), (2, 2, 3));
    assert_eq!(
        index.values().iter().copied().collect::<Vec<_>>(),
        vec![0., 1., 2., 0., 1., 2., 0., 1., 2., 0., 1., 2.]
    );
}

#[test]
fn explicit_float_precision_keeps_units_and_rejects_overflow() {
    let value: f64 = 11046.783203125 / (44101. / 2048.);
    assert_eq!(value.floor(), 512.);
    let input = Signal::vector(vec![value, -1e-50], Unit::Degrees).unwrap();
    let output = PreparedGraph::new(
        &standard_library(),
        "core/float32",
        &BTreeMap::from([("value".into(), Value::Signal(input))]),
        frame(),
    )
    .unwrap()
    .evaluate_batch(&[0.])
    .unwrap();
    let signal = output["value"].signal().unwrap();
    assert_eq!(signal.unit(), Unit::Degrees);
    assert_eq!(signal.values()[[0, 0, 0]], 513.);
    assert_eq!(signal.values()[[0, 0, 1]], 0.);
    assert!(PreparedGraph::new(
        &standard_library(),
        "core/float32",
        &BTreeMap::from([("value".into(), Value::Number(1e40))]),
        frame()
    )
    .is_err());
}
