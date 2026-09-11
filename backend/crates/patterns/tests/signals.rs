mod support;
use luma_patterns::*;
use std::collections::BTreeMap;
#[allow(unused_imports)]
use support::EvaluateEffect;

fn cells() -> Vec<Cell> {
    (0..16)
        .map(|i| Cell {
            id: format!("bar:{i}"),
            group: "bars".into(),
            world: [i as f64, 0., 0.],
            uvz: [i as f64, 0., 0.],
        })
        .collect()
}
fn frame(cells: &[Cell]) -> Frame<'_> {
    Frame {
        features: None,
        cells,
        beat: 3.0,
        clip_start: 3.0,
        clip_duration: 8.0,
        seed: 291,
    }
}

#[test]
fn fade_uses_placed_clip_duration_and_spatial_gradient_uses_mapping() {
    let library = standard_library();
    let cells = cells();
    let gradient = Value::Gradient(Gradient {
        stops: vec![
            ColorStop {
                alpha: 1.,
                t: 0.,
                color: [1., 0., 0.],
            },
            ColorStop {
                alpha: 1.,
                t: 1.,
                color: [0., 0., 1.],
            },
        ],
    });
    let fade = support::prepare_effect(
        &library,
        "gradient",
        &BTreeMap::from([("gradient".into(), gradient.clone())]),
        frame(&cells),
    )
    .unwrap();
    let sample = fade.evaluate(7.).unwrap();
    let Value::Lighting(light) = &sample["lighting"] else {
        panic!()
    };
    // Existing OKLab red/blue midpoint, sampled halfway through the placed
    // clip. Brightness is still carried by the color's maximum channel.
    for value in light.values() {
        let rgb = value.color.unwrap().map(|v| v * value.dimmer.unwrap());
        for (actual, expected) in rgb.into_iter().zip([0.550441, 0.325621, 0.636501]) {
            assert!((actual - expected).abs() < 1e-5, "{rgb:?}");
        }
    }
    assert_eq!(fade.evaluate(3.).unwrap(), fade.evaluate(3.).unwrap());
    let spatial = support::prepare_effect(
        &library,
        "spatial_gradient",
        &BTreeMap::from([
            ("gradient".into(), gradient),
            (
                "mapping".into(),
                Value::Mapping(MappingSpec {
                    mirror: None,
                    source: MappingSource::U,
                    per_group: false,
                    reverse: false,
                }),
            ),
        ]),
        frame(&cells),
    )
    .unwrap()
    .evaluate(7.)
    .unwrap();
    let Value::Lighting(light) = &spatial["lighting"] else {
        panic!()
    };
    assert_eq!(light["bar:0"].color, Some([1., 0., 0.]));
    assert_eq!(light["bar:15"].color, Some([0., 0., 1.]));
}

#[test]
fn pulse_is_dark_in_the_rest_even_when_the_curve_ends_lit() {
    let library = standard_library();
    let cells = cells();
    for effect in ["beat_pulse"] {
        let inputs = BTreeMap::from([(
            "shape".into(),
            Value::Envelope(Envelope::linear(vec![[0., 1.], [1., 1.]])),
        )]);
        let graph = support::prepare_effect(&library, effect, &inputs, frame(&cells)).unwrap();
        for (beat, dimmer) in [(3., 1.), (4.999, 1.), (5., 0.), (6.999, 0.), (7., 1.)] {
            let value = graph.evaluate(beat).unwrap();
            let Value::Lighting(light) = &value["lighting"] else {
                panic!()
            };
            assert!(light.values().all(|v| v.dimmer == Some(dimmer)));
            assert!(light.values().all(|v| v.color.is_none()));
        }
    }
}

#[test]
fn noise_is_smooth_seekable_and_bound_to_identity_and_seed() {
    let library = standard_library();
    let mut cells = cells();
    let graph =
        support::prepare_effect(&library, "noise_mask", &BTreeMap::new(), frame(&cells)).unwrap();
    let a = graph.evaluate(3.).unwrap();
    let b = graph.evaluate(3.001).unwrap();
    graph.evaluate(9.).unwrap();
    assert_eq!(a, graph.evaluate(3.).unwrap());
    let a = support::field(&a["mask"]);
    let b = support::field(&b["mask"]);
    assert!(a.iter().all(|(id, v)| (v - b[id]).abs() < 0.01));
    assert!(a.values().any(|v| *v != a["bar:0"]));
    cells.reverse();
    let reordered =
        support::prepare_effect(&library, "noise_mask", &BTreeMap::new(), frame(&cells)).unwrap();
    assert_eq!(graph.evaluate(8.).unwrap(), reordered.evaluate(8.).unwrap());
    let different = support::prepare_effect(
        &library,
        "noise_mask",
        &BTreeMap::new(),
        Frame {
            seed: 292,
            ..frame(&cells)
        },
    )
    .unwrap();
    assert_ne!(graph.evaluate(8.).unwrap(), different.evaluate(8.).unwrap());
}

#[test]
fn gradient_and_color_fields_reject_bad_authored_values_and_mismatched_domains() {
    let bad = Gradient {
        stops: vec![
            ColorStop {
                alpha: 1.,
                t: 0.5,
                color: [1.; 3],
            },
            ColorStop {
                alpha: 1.,
                t: 0.4,
                color: [0.; 3],
            },
        ],
    };
    assert!(bad.validate().is_err());
    let library = standard_library();
    let cells = cells();
    assert!(library
        .evaluate_effect(
            "mask_color",
            &BTreeMap::from([
                (
                    "color".into(),
                    Value::ColorField(BTreeMap::from([("a".into(), [1.; 3])]))
                ),
                (
                    "mask".into(),
                    Value::Mask(BTreeMap::from([("b".into(), 1.)]))
                ),
            ]),
            frame(&cells)
        )
        .is_err());
    for effect in [
        "wash",
        "gradient",
        "spatial_gradient",
        "beat_pulse",
        "noise_wash",
    ] {
        let mut score = Score::default();
        score
            .insert_effect(&library, effect, "clip", 3., 8.)
            .unwrap();
        score.validate(&library).unwrap();
        let prepared = score
            .prepare_clip(&library, "clip", &BTreeMap::new(), &cells)
            .unwrap();
        assert!(prepared.evaluate(4.).unwrap().contains_key("lighting"));
        assert!(matches!(library.definitions[effect].body, Body::Graph(_)));
    }
}

#[derive(Debug)]
struct Analysis;
impl FeatureSource for Analysis {
    fn onsets(&self, _drum: Drum) -> Result<EventTimes> {
        EventTimes::new(vec![4.0])
    }
    fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<FeatureSample> {
        Ok(match request {
            FeatureRequest::Timing => return Err(Error("test source has no timing".into())),
            FeatureRequest::Spectrum { .. } => {
                return Err(Error("test source has no spectrum".into()))
            }
            FeatureRequest::Band { .. } => FeatureSample::Energy(0.025),
            FeatureRequest::Onsets(_) => FeatureSample::Onset((beat >= 4.0).then_some((4.0, 0))),
            FeatureRequest::Harmony => FeatureSample::PitchClass(Some(9)),
        })
    }
}

#[test]
fn audio_sources_require_explicit_data_and_event_envelopes_seek_without_history() {
    let library = standard_library();
    let cells = cells();
    let program =
        support::prepare_effect(&library, "drum_pulse", &BTreeMap::new(), frame(&cells)).unwrap();
    assert_eq!(
        program.feature_requests(),
        &[FeatureRequest::Onsets(Drum::Kick)]
    );
    assert!(program
        .evaluate(4.0)
        .unwrap_err()
        .0
        .contains("analyzed track data"));
    let program = program
        .with_features(std::sync::Arc::new(Analysis))
        .unwrap();
    for (beat, dimmer) in [(4.5, 0.), (3.9, 0.), (4.0, 1.), (4.25, 0.5), (4.0, 1.)] {
        let frame = program.evaluate(beat).unwrap();
        let Value::Lighting(light) = &frame["lighting"] else {
            panic!()
        };
        assert!(light.values().all(|v| v.dimmer == Some(dimmer)));
    }
    let band = support::prepare_effect(&library, "band_pulse", &BTreeMap::new(), frame(&cells))
        .unwrap()
        .with_features(std::sync::Arc::new(Analysis))
        .unwrap();
    let sample = band.evaluate(5.).unwrap();
    let Value::Lighting(light) = &sample["lighting"] else {
        panic!()
    };
    assert!(light.values().all(|v| v.dimmer == Some(0.25)));
    let mut score = Score::default();
    score
        .insert_effect(&library, "drum_pulse", "kick", 3., 8.)
        .unwrap();
    score
        .clips
        .get_mut("kick")
        .unwrap()
        .inputs
        .insert("duration".into(), Value::Beats(0.));
    assert!(score.validate(&library).unwrap_err().0.contains("duration"));
    assert!(support::prepare_effect(
        &library,
        "band_energy",
        &BTreeMap::from([("high_hz".into(), Value::Number(10.))]),
        frame(&cells)
    )
    .is_err());
}
