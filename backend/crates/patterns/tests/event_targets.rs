use luma_patterns::*;
use ndarray::{array, Array2, Axis};

fn recorded(times: &[f64]) -> Events {
    Events::Beats {
        times: EventTimes::new(times.to_vec()).unwrap(),
    }
}
fn periodic() -> Events {
    Events::Periodic {
        repeat: 1.,
        grid_aligned: true,
        delay: 0.,
    }
}
fn flat() -> Envelope {
    Envelope::linear(vec![[0., 1.], [1., 1.]])
}

#[test]
fn an_earlier_head_fades_out_while_a_new_head_fades_in() {
    let targets =
        EventTargets::weights(vec!["b".into(), "a".into()], array![[0., 1.], [1., 0.]]).unwrap();
    let events = recorded(&[0., 0.5]).targeted(targets).unwrap();
    let shape = Envelope::linear(vec![[0., 0.], [0.25, 1.], [1., 0.]]);
    let signal = pulse_signal(&[0.75, 1., 2.5], &events, 0., 2., &shape).unwrap();
    assert_eq!(signal.fixtures().unwrap(), ["a", "b"]);
    assert_eq!(signal.values().dim(), (2, 3, 1));
    assert!((signal.values()[[0, 0, 0]] - 5. / 6.).abs() < 1e-12);
    assert!((signal.values()[[0, 1, 0]] - 2. / 3.).abs() < 1e-12);
    assert_eq!(signal.values()[[1, 0, 0]], 0.5);
    assert_eq!(signal.values()[[1, 1, 0]], 1.);
    assert_eq!(signal.values()[[0, 2, 0]], 0.);
    assert_eq!(signal.values()[[1, 2, 0]], 0.);
}

#[test]
fn repeated_selection_of_one_head_combines_envelopes_with_max() {
    let events = recorded(&[0., 0.5])
        .targeted(EventTargets::weights(vec!["a".into()], array![[1., 0.6]]).unwrap())
        .unwrap();
    let fade = Envelope::linear(vec![[0., 1.], [1., 0.]]);
    let actual = pulse_signal(&[0.5, 1., 1.5, 2., 2.5], &events, 0., 2., &fade).unwrap();
    for (i, expected) in [0.75, 0.5, 0.3, 0.15, 0.].into_iter().enumerate() {
        assert!((actual.values()[[0, i, 0]] - expected).abs() < 1e-12);
    }
}

#[test]
fn random_subsets_are_exact_and_independent_of_seek_and_domain_order() {
    let ids: Vec<_> = (0..8).map(|i| format!("head-{i}")).collect();
    let source = periodic()
        .targeted(EventTargets::random(ids.clone(), 0.375, 427, false).unwrap())
        .unwrap();
    let times = [3.1, -2.9, 0.1, 3.1, 1_000_000.1, 2.1, 1.1];
    let batch = pulse_signal(&times, &source, 0., 0.2, &flat()).unwrap();
    for (t, time) in times.iter().enumerate() {
        let single = pulse_signal(&[*time], &source, 0., 0.2, &flat()).unwrap();
        let column = batch.values().index_axis(Axis(1), t);
        assert_eq!(column, single.values().index_axis(Axis(1), 0));
        assert_eq!(column.sum(), 3.);
    }
    assert!((1..times.len())
        .any(|t| batch.values().index_axis(Axis(1), t) != batch.values().index_axis(Axis(1), 0)));
    let reversed = periodic()
        .targeted(EventTargets::random(ids.into_iter().rev().collect(), 0.375, 427, false).unwrap())
        .unwrap();
    assert_eq!(
        batch,
        pulse_signal(&times, &reversed, 0., 0.2, &flat()).unwrap()
    );
    assert_eq!(
        pulse_signal(&[], &source, 0., 0.2, &flat())
            .unwrap()
            .values()
            .dim(),
        (8, 0, 1)
    );
}

#[test]
fn random_selection_keeps_old_cohorts_until_their_envelopes_finish() {
    let ids: Vec<_> = (0..12).map(|i| format!("head-{i:02}")).collect();
    let events = recorded(&[0., 1., 2.]);
    let selected = events
        .clone()
        .targeted(EventTargets::random(ids.clone(), 0.25, 99, false).unwrap())
        .unwrap();
    // Capture membership separately at each start, using non-overlapping flat pulses.
    let membership = pulse_signal(&[0., 1., 2.], &selected, 0., 0.1, &flat()).unwrap();
    let shape = Envelope::linear(vec![[0., 0.], [0.25, 1.], [1., 0.]]);
    let times = [1.25, 2.25, 0.25, 4., 1.25];
    let output = pulse_signal(&times, &selected, 0., 2., &shape).unwrap();
    for h in 0..ids.len() {
        for (t, time) in times.iter().enumerate() {
            let expected = (0..3)
                .map(|e| {
                    let age = (time - e as f64) / 2.;
                    if (0. ..1.).contains(&age) {
                        membership.values()[[h, e, 0]] * shape.sample(age)
                    } else {
                        0.
                    }
                })
                .fold(0., f64::max);
            assert!((output.values()[[h, t, 0]] - expected).abs() < 1e-12);
        }
    }
}

#[test]
fn shuffled_cycles_minimize_repeats_without_replaying_earlier_events() {
    for heads in [0, 1, 5, 8] {
        let ids: Vec<_> = (0..heads).map(|i| format!("head-{i}")).collect();
        for count in 0..=heads {
            let proportion = count as f64 / heads.max(1) as f64;
            let events = periodic()
                .targeted(EventTargets::random(ids.clone(), proportion, 813, true).unwrap())
                .unwrap();
            let times = [-1e12, -1e12 + 1., 0., 1., 2., 1e12, 1e12 + 1.];
            let batch = pulse_signal(&times, &events, 0., 0.5, &flat()).unwrap();
            for (t, time) in times.iter().enumerate() {
                let single = pulse_signal(&[*time], &events, 0., 0.5, &flat()).unwrap();
                assert_eq!(
                    batch.values().index_axis(Axis(1), t),
                    single.values().index_axis(Axis(1), 0)
                );
                assert_eq!(single.values().sum(), count as f64);
            }
            for (a, b) in [(0, 1), (2, 3), (3, 4), (5, 6)] {
                let overlap = (0..heads)
                    .filter(|n| batch.values()[[*n, a, 0]] > 0. && batch.values()[[*n, b, 0]] > 0.)
                    .count();
                assert_eq!(overlap, (2 * count).saturating_sub(heads));
            }
            let reversed = periodic()
                .targeted(
                    EventTargets::random(
                        ids.iter().rev().cloned().collect(),
                        proportion,
                        813,
                        true,
                    )
                    .unwrap(),
                )
                .unwrap();
            assert_eq!(
                batch,
                pulse_signal(&times, &reversed, 0., 0.5, &flat()).unwrap()
            );
            let roundtrip: Events =
                serde_json::from_str(&serde_json::to_string(&events).unwrap()).unwrap();
            assert_eq!(events, roundtrip);
        }
    }
}

#[test]
fn targeting_is_shared_by_chase_and_can_filter_an_existing_selection() {
    let initial = recorded(&[0.])
        .targeted(
            EventTargets::weights(
                vec!["a".into(), "b".into(), "c".into(), "d".into()],
                array![[1.], [0.], [1.], [0.]],
            )
            .unwrap(),
        )
        .unwrap();
    let subset = initial
        .targeted(
            EventTargets::random(
                vec!["d".into(), "c".into(), "b".into(), "a".into()],
                0.5,
                1,
                false,
            )
            .unwrap(),
        )
        .unwrap();
    let pulse = pulse_signal(&[0.5], &subset, 0., 1., &flat()).unwrap();
    assert_eq!(pulse.values().sum(), 1.);
    assert_eq!(pulse.values()[[1, 0, 0]], 0.);
    assert_eq!(pulse.values()[[3, 0, 0]], 0.);
    let mapping = Mapping::linear(
        [
            ("a".into(), 0.),
            ("b".into(), 0.3),
            ("c".into(), 0.6),
            ("d".into(), 1.),
        ],
        false,
    )
    .unwrap();
    let chase = chase_signal(
        &mapping,
        &[0.5],
        &subset,
        0.,
        ChaseShape {
            travel: 1.,
            start: 0.,
            end: 1.,
            path: &Envelope::linear(vec![[0., 0.], [1., 1.]]),
            width: 1.,
            shape: &flat(),
            boundary: Boundary::Wrap,
        },
    )
    .unwrap();
    assert_eq!(pulse, chase);
}

#[test]
fn targeting_validates_identity_shapes_and_serialization() {
    assert!(EventTargets::random(vec!["a".into(), "a".into()], 0.5, 1, false).is_err());
    assert!(EventTargets::random(vec!["a".into()], f64::NAN, 1, false).is_err());
    assert!(EventTargets::weights(vec!["a".into()], array![[2.]]).is_err());
    assert!(recorded(&[0., 1.])
        .targeted(EventTargets::weights(vec!["a".into()], array![[1.]]).unwrap())
        .is_err());
    assert!(Events::Automatic
        .targeted(EventTargets::random(vec![], 0.5, 0, false).unwrap())
        .is_err());
    let events = recorded(&[0., 1.])
        .targeted(EventTargets::weights(vec!["a".into()], array![[1., 0.5]]).unwrap())
        .unwrap();
    let saved = serde_json::to_string(&events).unwrap();
    assert_eq!(events, serde_json::from_str::<Events>(&saved).unwrap());
    let empty = recorded(&[])
        .targeted(EventTargets::weights(vec![], Array2::zeros((0, 0))).unwrap())
        .unwrap();
    assert_eq!(
        pulse_signal(&[1., 2.], &empty, 0., 1., &flat())
            .unwrap()
            .values()
            .dim(),
        (0, 2, 1)
    );
    for proportion in [0., 1.] {
        let events = periodic()
            .targeted(EventTargets::random(vec!["a".into()], proportion, 0, false).unwrap())
            .unwrap();
        assert_eq!(
            pulse_signal(&[0.], &events, 0., 1., &flat())
                .unwrap()
                .values()[[0, 0, 0]],
            proportion
        );
    }
}
