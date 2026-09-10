use super::*;

#[test]
fn clip_ranges_preserve_single_reductions_and_correct_nested_reductions() {
    let fixtures: Vec<Reference> =
        serde_json::from_str(include_str!("../../fixtures/reductions-v1.json")).unwrap();
    assert_eq!(fixtures.iter().map(|f| f.cases.len()).sum::<usize>(), 160);
    let mut largest = BTreeMap::<String, f64>::new();
    let mut nested_corrections = 0;
    for fixture in fixtures {
        let clock = fixture.grid.timeline().unwrap();
        let beats = fixture
            .times
            .iter()
            .map(|t| clock.beat_at(f64::from(*t)).unwrap())
            .collect::<Vec<_>>();
        let features = std::sync::Arc::new(ReferenceFeatures {
            clock: clock.clone(),
            timing: std::sync::Arc::new(fixture.grid.timing().unwrap()),
            onsets: Default::default(),
            harmony: vec![],
            audio: Default::default(),
        });
        let mut actual = BTreeMap::<String, Vec<f64>>::new();
        for case in &fixture.cases {
            let score = convert(&case.graph, &case.name).unwrap();
            let library = score.library(&p::standard_library()).unwrap();
            let program = p::PreparedGraph::new(
                &library,
                ROOT,
                &BTreeMap::new(),
                p::Frame {
                    cells: &fixture.cells,
                    features: None,
                    beat: fixture.clip_start,
                    clip_start: fixture.clip_start,
                    clip_duration: fixture.clip_duration,
                    seed: 0,
                },
            )
            .unwrap()
            .with_features(features.clone())
            .unwrap();
            let batch = program.evaluate_batch(&beats).unwrap();
            let signal = batch["view/view"].signal().unwrap();
            let reference = &case.views["view"];
            assert_eq!(signal.values().dim().0, reference.n);
            assert_eq!(signal.values().dim().2, reference.c);
            let mut values = Vec::new();
            for n in 0..reference.n {
                for t in 0..reference.t {
                    for c in 0..reference.c {
                        values.push(signal.values()[[n, t.min(signal.values().dim().1 - 1), c]]);
                    }
                }
            }
            for (t, beat) in beats.iter().enumerate() {
                let sought = program.evaluate_batch(&[*beat]).unwrap();
                assert_eq!(
                    batch["lighting"].lighting().unwrap().sample(t).unwrap(),
                    sought["lighting"].lighting().unwrap().sample(0).unwrap()
                );
            }
            if case.name.matches('/').count() == 1 {
                let error = values
                    .iter()
                    .zip(&reference.data)
                    .map(|(a, b)| (a - f64::from(*b)).abs())
                    .fold(0., f64::max);
                largest
                    .entry(case.name.clone())
                    .and_modify(|e| *e = e.max(error))
                    .or_insert(error);
            } else {
                assert!(
                    reference.data.iter().all(|v| *v == 0.),
                    "{}: original nested-range bug changed",
                    case.name
                );
            }
            actual.insert(case.name.clone(), values);
        }
        for (name, values) in &actual {
            if name.matches('/').count() != 2 {
                continue;
            }
            let source = name.split('/').next().unwrap();
            let first = &actual[&format!("{source}/normalize")];
            for (value, first) in values.iter().zip(first) {
                let expected = if ["scalar", "noise"].contains(&source) {
                    0.
                } else if name.ends_with("/invert") {
                    1. - first
                } else {
                    *first
                };
                assert!(
                    (value - expected).abs() < 1e-10,
                    "{name}: {value} != {expected}"
                );
                nested_corrections += usize::from(value.abs() > 1e-5);
            }
        }
    }
    eprintln!("single-reduction maximum differences: {largest:?}");
    assert!(nested_corrections > 100);
    for (name, error) in largest {
        assert!(
            error < 0.002,
            "{name}: range sampling changed output by {error}"
        );
    }
}
