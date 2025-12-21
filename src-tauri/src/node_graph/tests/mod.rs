use super::executor::{adsr_durations, calc_envelope};
use super::{
    run_graph_internal, BeatGrid, Edge, Graph, GraphContext, GraphExecutionConfig, NodeInstance,
    RunResult, Signal,
};
use crate::audio::StemCache;
use serde_json::json;
use sqlx::SqlitePool;

    fn run(graph: Graph) -> RunResult {
        tauri::async_runtime::block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:")
                .await
                .expect("in-memory db");
            // Dummy context for tests that don't use audio_input nodes
            let context = GraphContext {
                track_id: 0,
                start_time: 0.0,
                end_time: 0.0,
                beat_grid: None,
                arg_values: None,
            };
            let stem_cache = StemCache::new();
            let fft_service = crate::audio::FftService::new();
            // Ignore the layer output for this test wrapper
            let (result, _) = run_graph_internal(
                &pool,
                None,
                &stem_cache,
                &fft_service,
                None,
                graph,
                context,
                GraphExecutionConfig::default(),
            )
            .await
            .expect("graph execution should succeed");
            result
        })
    }

    #[test]
    fn adsr_durations_span_fills_full_interval() {
        // Attack of 1.0 with no other phases should span the full interval.
        let (att, dec, sus, rel) = adsr_durations(2.0, 1.0, 0.0, 0.0, 0.0);
        assert!((att - 2.0).abs() < 1e-6);
        assert!(dec.abs() < 1e-6 && sus.abs() < 1e-6 && rel.abs() < 1e-6);
    }

    #[test]
    fn calc_envelope_peak_is_not_a_drop_to_zero() {
        // If we sample exactly at the peak and later phases are 0 duration,
        // we should not fall through to 0.0.
        let peak = 10.0;
        let attack = 2.0;
        let decay = 0.0;
        let sustain = 0.0;
        let release = 0.0;
        let sustain_level = 0.0;
        let a_curve = 0.0;
        let d_curve = 0.0;

        let just_before = calc_envelope(
            peak - 1e-3,
            peak,
            attack,
            decay,
            sustain,
            release,
            sustain_level,
            a_curve,
            d_curve,
        );
        let at_peak = calc_envelope(
            peak,
            peak,
            attack,
            decay,
            sustain,
            release,
            sustain_level,
            a_curve,
            d_curve,
        );

        assert!(just_before > 0.99, "just_before={just_before}");
        assert!((at_peak - 1.0).abs() < 1e-6, "at_peak={at_peak}");
    }

    fn run_with_context(graph: Graph, context: GraphContext) -> RunResult {
        tauri::async_runtime::block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:")
                .await
                .expect("in-memory db");
            let stem_cache = StemCache::new();
            let fft_service = crate::audio::FftService::new();
            let (result, _) = run_graph_internal(
                &pool,
                None,
                &stem_cache,
                &fft_service,
                None,
                graph,
                context,
                GraphExecutionConfig::default(),
            )
            .await
            .expect("graph execution should succeed");
            result
        })
    }

    #[test]
    fn beat_envelope_drops_start_pulse_for_attack_to_avoid_initial_peak_drop() {
        // When attack is non-zero, a pulse at exactly start_time creates a visible 1->0 drop
        // at the beginning of the segment. If another pulse exists later, we drop the start pulse
        // so the envelope ramps toward the next one.
        let beat_grid = BeatGrid {
            beats: vec![0.0, 1.0],
            downbeats: vec![0.0, 1.0],
            bpm: 60.0,
            downbeat_offset: 0.0,
            beats_per_bar: 4,
        };

        let mut params = std::collections::HashMap::new();
        params.insert("subdivision".into(), json!(1.0));
        params.insert("only_downbeats".into(), json!(0.0));
        params.insert("offset".into(), json!(0.0));
        params.insert("attack".into(), json!(1.0));
        params.insert("decay".into(), json!(0.0));
        params.insert("sustain".into(), json!(0.0));
        params.insert("release".into(), json!(0.0));
        params.insert("sustain_level".into(), json!(0.0));
        params.insert("attack_curve".into(), json!(0.0));
        params.insert("decay_curve".into(), json!(0.0));
        params.insert("amplitude".into(), json!(1.0));

        let graph = Graph {
            nodes: vec![
                NodeInstance {
                    id: "env".into(),
                    type_id: "beat_envelope".into(),
                    params,
                    position_x: None,
                    position_y: None,
                },
                NodeInstance {
                    id: "view".into(),
                    type_id: "view_signal".into(),
                    params: std::collections::HashMap::new(),
                    position_x: None,
                    position_y: None,
                },
            ],
            edges: vec![Edge {
                id: "e1".into(),
                from_node: "env".into(),
                from_port: "out".into(),
                to_node: "view".into(),
                to_port: "in".into(),
            }],
            args: vec![],
        };

        let result = run_with_context(
            graph,
            GraphContext {
                track_id: 0,
                start_time: 0.0,
                end_time: 1.0,
                beat_grid: Some(beat_grid),
                arg_values: None,
            },
        );

        let sig = result.views.get("view").expect("view signal exists");
        let first = sig.data.first().copied().unwrap_or(0.0);
        let last = sig.data.last().copied().unwrap_or(0.0);
        assert!(
            first.abs() < 1e-6,
            "expected first sample to start low (0.0), got {first}"
        );
        assert!(
            last > 0.9,
            "expected last sample to be near peak (1.0), got {last}"
        );
    }

    #[test]
    fn beat_envelope_does_not_spike_at_segment_end_for_decay_only() {
        // If a beat lands exactly at end_time, we sample end-exclusive and drop the end pulse
        // so the last sample doesn't jump back to 1.0.
        let beat_grid = BeatGrid {
            beats: vec![0.0, 1.0],
            downbeats: vec![0.0, 1.0],
            bpm: 60.0,
            downbeat_offset: 0.0,
            beats_per_bar: 4,
        };

        let mut params = std::collections::HashMap::new();
        params.insert("subdivision".into(), json!(1.0));
        params.insert("only_downbeats".into(), json!(0.0));
        params.insert("offset".into(), json!(0.0));
        params.insert("attack".into(), json!(0.0));
        params.insert("decay".into(), json!(1.0));
        params.insert("sustain".into(), json!(0.0));
        params.insert("release".into(), json!(0.0));
        params.insert("sustain_level".into(), json!(0.5));
        params.insert("attack_curve".into(), json!(0.0));
        params.insert("decay_curve".into(), json!(0.0));
        params.insert("amplitude".into(), json!(1.0));

        let graph = Graph {
            nodes: vec![
                NodeInstance {
                    id: "env".into(),
                    type_id: "beat_envelope".into(),
                    params,
                    position_x: None,
                    position_y: None,
                },
                NodeInstance {
                    id: "view".into(),
                    type_id: "view_signal".into(),
                    params: std::collections::HashMap::new(),
                    position_x: None,
                    position_y: None,
                },
            ],
            edges: vec![Edge {
                id: "e1".into(),
                from_node: "env".into(),
                from_port: "out".into(),
                to_node: "view".into(),
                to_port: "in".into(),
            }],
            args: vec![],
        };

        let result = run_with_context(
            graph,
            GraphContext {
                track_id: 0,
                start_time: 0.0,
                end_time: 1.0,
                beat_grid: Some(beat_grid),
                arg_values: None,
            },
        );

        let sig = result.views.get("view").expect("view signal exists");
        let last = sig.data.last().copied().unwrap_or(0.0);
        assert!(
            last < 0.75,
            "expected last sample to remain near sustain (0.5), got {last}"
        );
    }

    #[test]
    fn beat_envelope_attack_decay_does_not_flatline_at_segment_start() {
        // Regression: the "drop start pulse" fix should only apply to attack-only shapes.
        // For attack+decay, the start pulse is needed so the segment starts in decay, not 0.
        let beat_grid = BeatGrid {
            beats: vec![0.0, 1.0],
            downbeats: vec![0.0, 1.0],
            bpm: 60.0,
            downbeat_offset: 0.0,
            beats_per_bar: 4,
        };

        let mut params = std::collections::HashMap::new();
        params.insert("subdivision".into(), json!(1.0));
        params.insert("only_downbeats".into(), json!(0.0));
        params.insert("offset".into(), json!(0.0));
        params.insert("attack".into(), json!(0.5));
        params.insert("decay".into(), json!(0.5));
        params.insert("sustain".into(), json!(0.0));
        params.insert("release".into(), json!(0.0));
        params.insert("sustain_level".into(), json!(0.0));
        params.insert("attack_curve".into(), json!(0.0));
        params.insert("decay_curve".into(), json!(0.0));
        params.insert("amplitude".into(), json!(1.0));

        let graph = Graph {
            nodes: vec![
                NodeInstance {
                    id: "env".into(),
                    type_id: "beat_envelope".into(),
                    params,
                    position_x: None,
                    position_y: None,
                },
                NodeInstance {
                    id: "view".into(),
                    type_id: "view_signal".into(),
                    params: std::collections::HashMap::new(),
                    position_x: None,
                    position_y: None,
                },
            ],
            edges: vec![Edge {
                id: "e1".into(),
                from_node: "env".into(),
                from_port: "out".into(),
                to_node: "view".into(),
                to_port: "in".into(),
            }],
            args: vec![],
        };

        let result = run_with_context(
            graph,
            GraphContext {
                track_id: 0,
                start_time: 0.0,
                end_time: 1.0,
                beat_grid: Some(beat_grid),
                arg_values: None,
            },
        );

        let sig = result.views.get("view").expect("view signal exists");
        let first = sig.data.first().copied().unwrap_or(0.0);
        assert!(
            first > 0.9,
            "expected segment to start near peak (decay from start pulse), got {first}"
        );
    }

    #[test]
    fn test_tensor_broadcasting_logic() {
        // Signal A: Spatial (N=4, T=1, C=1) -> [0, 1, 2, 3]
        let sig_a = Signal {
            n: 4,
            t: 1,
            c: 1,
            data: vec![0.0, 1.0, 2.0, 3.0],
        };

        // Signal B: Temporal (N=1, T=2, C=1) -> [10, 20]
        let sig_b = Signal {
            n: 1,
            t: 2,
            c: 1,
            data: vec![10.0, 20.0],
        };

        // Emulate the Math node logic
        let out_n = sig_a.n.max(sig_b.n);
        let out_t = sig_a.t.max(sig_b.t);
        let out_c = sig_a.c.max(sig_b.c);

        let mut result_data = Vec::new();

        for i in 0..out_n {
            let idx_a_n = if sig_a.n == 1 { 0 } else { i % sig_a.n };
            let idx_b_n = if sig_b.n == 1 { 0 } else { i % sig_b.n };

            for j in 0..out_t {
                let idx_a_t = if sig_a.t == 1 { 0 } else { j % sig_a.t };
                let idx_b_t = if sig_b.t == 1 { 0 } else { j % sig_b.t };

                for k in 0..out_c {
                    let idx_a_c = if sig_a.c == 1 { 0 } else { k % sig_a.c };
                    let idx_b_c = if sig_b.c == 1 { 0 } else { k % sig_b.c };

                    let flat_a = idx_a_n * (sig_a.t * sig_a.c) + idx_a_t * sig_a.c + idx_a_c;
                    let flat_b = idx_b_n * (sig_b.t * sig_b.c) + idx_b_t * sig_b.c + idx_b_c;

                    let val_a = sig_a.data.get(flat_a).copied().unwrap_or(0.0);
                    let val_b = sig_b.data.get(flat_b).copied().unwrap_or(0.0);

                    result_data.push(val_a + val_b);
                }
            }
        }

        assert_eq!(
            result_data,
            vec![10.0, 20.0, 11.0, 21.0, 12.0, 22.0, 13.0, 23.0]
        );
    }
