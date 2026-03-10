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
            venue_id: 0,
            start_time: 0.0,
            end_time: 0.0,
            beat_grid: None,
            arg_values: None,
            instance_seed: None,
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
fn calc_envelope_shape_ramps_correctly() {
    // t is offset from shape start. Attack ramps 0→1 over att_s seconds.
    let att = 2.0;
    let dec = 0.0;
    let sus = 0.0;
    let rel = 0.0;
    let sustain_level = 0.0;

    let at_start = calc_envelope(0.0, att, dec, sus, rel, sustain_level, 0.0, 0.0);
    let at_mid = calc_envelope(1.0, att, dec, sus, rel, sustain_level, 0.0, 0.0);
    let near_end = calc_envelope(att - 1e-3, att, dec, sus, rel, sustain_level, 0.0, 0.0);

    assert!(at_start.abs() < 1e-6, "at_start={at_start}");
    assert!((at_mid - 0.5).abs() < 0.01, "at_mid={at_mid}");
    assert!(near_end > 0.99, "near_end={near_end}");
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
fn beat_envelope_attack_starts_at_beat() {
    // Attack phase begins at the beat time, so the first sample should be near 0
    // (start of attack ramp) and peak arrives at the end of the attack phase.
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
            venue_id: 0,
            start_time: 0.0,
            end_time: 1.0,
            beat_grid: Some(beat_grid),
            arg_values: None,
            instance_seed: None,
        },
    );

    let sig = result.views.get("view").expect("view signal exists");
    let first = sig.data.first().copied().unwrap_or(1.0);
    let last = sig.data.last().copied().unwrap_or(0.0);
    assert!(
        first < 0.1,
        "expected first sample near 0 (start of attack ramp), got {first}"
    );
    assert!(
        last > 0.9,
        "expected last sample near peak (1.0), got {last}"
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
            venue_id: 0,
            start_time: 0.0,
            end_time: 1.0,
            beat_grid: Some(beat_grid),
            arg_values: None,
            instance_seed: None,
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
fn beat_envelope_attack_decay_starts_with_ramp() {
    // With attack+decay and a pulse at t=0, the attack starts at t=0 and the
    // peak arrives at t=att_s. The first sample should be near 0 (start of ramp).
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
            venue_id: 0,
            start_time: 0.0,
            end_time: 1.0,
            beat_grid: Some(beat_grid),
            arg_values: None,
            instance_seed: None,
        },
    );

    let sig = result.views.get("view").expect("view signal exists");
    let first = sig.data.first().copied().unwrap_or(1.0);
    assert!(
        first < 0.1,
        "expected segment to start near 0 (start of attack ramp), got {first}"
    );
}

#[test]
fn beat_envelope_subdivision_half_offset_one_on_beat_four() {
    // Reproduces the bug: subdivision=0.5 (pulse every 2 beats), offset=1,
    // annotation on beat 4/4 of bar 1 (beat index 3, spanning one beat).
    // The signal should NOT be all zeros.
    let bpm = 120.0;
    let beat_len = 60.0 / bpm; // 0.5s
                               // Full beat grid: beats 0..7 (two bars of 4/4)
    let beats: Vec<f32> = (0..8).map(|i| i as f32 * beat_len).collect();
    let downbeats: Vec<f32> = vec![0.0, 4.0 * beat_len];
    let beat_grid = BeatGrid {
        beats: beats.clone(),
        downbeats,
        bpm,
        downbeat_offset: 0.0,
        beats_per_bar: 4,
    };

    let mut params = std::collections::HashMap::new();
    params.insert("subdivision".into(), json!(0.5));
    params.insert("only_downbeats".into(), json!(0.0));
    params.insert("offset".into(), json!(1.0));
    params.insert("attack".into(), json!(0.3));
    params.insert("decay".into(), json!(0.2));
    params.insert("sustain".into(), json!(0.3));
    params.insert("release".into(), json!(0.2));
    params.insert("sustain_level".into(), json!(0.7));
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

    // Annotation spans beat 3 to beat 4 (one beat on the 4th beat of bar 1)
    let start_time = 3.0 * beat_len; // 1.5s
    let end_time = 4.0 * beat_len; // 2.0s

    let result = run_with_context(
        graph,
        GraphContext {
            track_id: 0,
            venue_id: 0,
            start_time,
            end_time,
            beat_grid: Some(beat_grid),
            arg_values: None,
            instance_seed: None,
        },
    );

    let sig = result.views.get("view").expect("view signal exists");
    let max_val = sig.data.iter().cloned().fold(0.0f32, f32::max);
    assert!(
        max_val > 0.1,
        "expected non-zero signal for subdivision=0.5 offset=1 on beat 4/4, but max was {max_val}"
    );
}

#[test]
fn beat_envelope_subdivision_half_offset_one_sliced_grid() {
    // Sliced grid with padding: includes beats before the annotation so that
    // subdivision/offset combinations that derive pulses from earlier beats
    // still produce output within the annotation window.
    let bpm = 120.0;
    let beat_len = 60.0 / bpm;
    let start_time = 3.0 * beat_len;
    let end_time = 4.0 * beat_len;

    // Padded grid: include beats from the start of bar 1 through end of annotation
    let beats: Vec<f32> = (0..=4).map(|i| i as f32 * beat_len).collect();
    let beat_grid = BeatGrid {
        beats,
        downbeats: vec![0.0],
        bpm,
        downbeat_offset: 0.0,
        beats_per_bar: 4,
    };

    let mut params = std::collections::HashMap::new();
    params.insert("subdivision".into(), json!(0.5));
    params.insert("only_downbeats".into(), json!(0.0));
    params.insert("offset".into(), json!(1.0));
    params.insert("attack".into(), json!(0.3));
    params.insert("decay".into(), json!(0.2));
    params.insert("sustain".into(), json!(0.3));
    params.insert("release".into(), json!(0.2));
    params.insert("sustain_level".into(), json!(0.7));
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
            venue_id: 0,
            start_time,
            end_time,
            beat_grid: Some(beat_grid),
            arg_values: None,
            instance_seed: None,
        },
    );

    let sig = result.views.get("view").expect("view signal exists");
    let max_val = sig.data.iter().cloned().fold(0.0f32, f32::max);
    assert!(
        max_val > 0.1,
        "expected non-zero signal with sliced grid, but max was {max_val}"
    );
}

#[test]
fn beat_envelope_z_chase_exact_reproduction() {
    // Exact reproduction of the z-chase pattern on "Gimme Some Keys"
    // BPM=125, downbeat_offset=0.02, annotation on beat 4/4 of bar 26
    // subdivision=0.5 (from pattern_args signal), beat_offset=1 (from signal)
    let bpm = 125.0_f32;
    let beat_len = 60.0 / bpm; // 0.48
    let downbeat_offset = 0.02_f32;
    let beats: Vec<f32> = (0..353)
        .map(|i| downbeat_offset + i as f32 * beat_len)
        .collect();
    let downbeats: Vec<f32> = (0..89)
        .map(|i| downbeat_offset + i as f32 * beat_len * 4.0)
        .collect();
    let beat_grid = BeatGrid {
        beats: beats.clone(),
        downbeats,
        bpm,
        downbeat_offset,
        beats_per_bar: 4,
    };

    // Annotation: beat index 103 to 104
    let start_time = beats[103]; // ~49.46
    let end_time = beats[104]; // ~49.94

    // The beat_envelope node has these params (from DB),
    // but subdivision and offset come via signal inputs from pattern_args
    let mut params = std::collections::HashMap::new();
    params.insert("subdivision".into(), json!(1.0)); // overridden by signal
    params.insert("only_downbeats".into(), json!(0.0));
    params.insert("offset".into(), json!(0.0)); // overridden by signal
    params.insert("attack".into(), json!(0.8));
    params.insert("decay".into(), json!(0.0));
    params.insert("sustain".into(), json!(0.72));
    params.insert("release".into(), json!(0.0));
    params.insert("sustain_level".into(), json!(1.0));
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

    // Test with the FULL beat grid (no slicing) and default params
    // (no signal inputs -- subdivision=1, offset=0 from params).
    // This simulates what happens without pattern_args signal connections.
    let result = run_with_context(
        graph.clone(),
        GraphContext {
            track_id: 0,
            venue_id: 0,
            start_time,
            end_time,
            beat_grid: Some(beat_grid.clone()),
            arg_values: None,
            instance_seed: None,
        },
    );

    let sig = result.views.get("view").expect("view signal exists");
    let max_val = sig.data.iter().cloned().fold(0.0f32, f32::max);
    // With subdivision=1 and offset=0, there should be a pulse at every beat,
    // including beat 103 (start_time). Signal should be non-zero.
    assert!(
        max_val > 0.1,
        "expected non-zero with subdivision=1 offset=0, got max={max_val}"
    );

    // Now test with params matching the runtime overrides (subdivision=0.5, offset=1)
    // but WITHOUT signal inputs (just setting them as node params directly).
    let mut params2 = std::collections::HashMap::new();
    params2.insert("subdivision".into(), json!(0.5));
    params2.insert("only_downbeats".into(), json!(0.0));
    params2.insert("offset".into(), json!(1.0));
    params2.insert("attack".into(), json!(0.8));
    params2.insert("decay".into(), json!(0.0));
    params2.insert("sustain".into(), json!(0.72));
    params2.insert("release".into(), json!(0.0));
    params2.insert("sustain_level".into(), json!(1.0));
    params2.insert("attack_curve".into(), json!(0.0));
    params2.insert("decay_curve".into(), json!(0.0));
    params2.insert("amplitude".into(), json!(1.0));

    let graph2 = Graph {
        nodes: vec![
            NodeInstance {
                id: "env".into(),
                type_id: "beat_envelope".into(),
                params: params2,
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

    let result2 = run_with_context(
        graph2,
        GraphContext {
            track_id: 0,
            venue_id: 0,
            start_time,
            end_time,
            beat_grid: Some(beat_grid),
            arg_values: None,
            instance_seed: None,
        },
    );

    let sig2 = result2.views.get("view").expect("view signal exists");
    let max_val2 = sig2.data.iter().cloned().fold(0.0f32, f32::max);
    assert!(
        max_val2 > 0.1,
        "expected non-zero with subdivision=0.5 offset=1 on full grid, got max={max_val2}"
    );

    // Verify the previous pulse's sustain carries into the annotation start.
    // With sustain_level=1.0 and attack=0.8 (longer than annotation duration),
    // the first sample should be at sustain_level from the previous pulse, not 0.
    let first2 = sig2.data.first().copied().unwrap_or(0.0);
    assert!(
        first2 >= 0.99,
        "expected first sample near sustain_level=1.0 (previous pulse tail), got {first2}"
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
