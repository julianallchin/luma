//! Cue compilation for the live controller layer.
//!
//! All cues compile for the full track duration — loop patterns repeat naturally
//! in the buffer, TrackTime patterns use audio analysis. Both sample at deck_time.
//! Called alongside `render_composite_deck` — never at pad-press time.

use std::collections::HashMap;

use sqlx::SqlitePool;

use crate::audio::{FftService, StemCache};
use crate::compositor::{
    fetch_pattern_graph, fetch_track_path_and_hash, get_or_load_shared_audio, get_track_duration,
    load_beat_grid,
};
use crate::models::midi::{Cue, CueExecutionMode};
use crate::models::node_graph::{BeatGrid, Graph, GraphContext};
use crate::node_graph::{run_graph_internal, GraphExecutionConfig};
use crate::render_engine::{CompiledCue, CompiledCueMode, RenderEngine, SIM_DECK_ID};

/// Node type IDs that require audio data during compilation.
const TRACK_TIME_NODE_TYPES: &[&str] = &[
    "harmony_analysis",
    "audio_input",
    "frequency_amplitude",
    "beat_input",
];

/// BPM / duration for the always-running simulated deck.
pub const SIM_BPM: f32 = 120.0;
pub const SIM_BEATS_PER_BAR: i32 = 4;
pub const SIM_DURATION: f32 = 30.0; // must match SIM_DECK_DURATION in render_engine

/// Returns true if the graph contains any nodes that require audio analysis data.
pub fn graph_requires_track_time(graph: &Graph) -> bool {
    graph
        .nodes
        .iter()
        .any(|n| TRACK_TIME_NODE_TYPES.contains(&n.type_id.as_str()))
}

/// Compile all cues for a venue onto a specific deck.
/// Called after `render_composite_deck` completes for `deck_id`.
/// All cues compile for the full track duration — sample at deck_time.
pub async fn compile_cues_for_deck(
    pool: &SqlitePool,
    stem_cache: &StemCache,
    fft_service: &FftService,
    resource_path_root: Option<std::path::PathBuf>,
    render_engine: &RenderEngine,
    deck_id: u8,
    track_id: &str,
    venue_id: &str,
) -> Result<(), String> {
    let cues = crate::database::local::midi::list_cues(pool, venue_id).await?;
    if cues.is_empty() {
        return Ok(());
    }

    // Load shared resources once
    let beat_grid = load_beat_grid(pool, track_id).await?;
    let track_duration = get_track_duration(pool, track_id).await?.unwrap_or(300.0);
    let (track_path, track_hash) = fetch_track_path_and_hash(pool, track_id).await?;
    let shared_audio = get_or_load_shared_audio(track_id, &track_path, &track_hash)
        .await
        .ok();

    let bpm = beat_grid.as_ref().map(|bg| bg.bpm).unwrap_or(120.0);
    let beats_per_bar = beat_grid.as_ref().map(|bg| bg.beats_per_bar).unwrap_or(4);

    for cue in &cues {
        match compile_single_cue(
            pool,
            stem_cache,
            fft_service,
            resource_path_root.clone(),
            cue,
            track_id,
            venue_id,
            &beat_grid,
            bpm,
            beats_per_bar,
            track_duration,
            shared_audio.clone(),
        )
        .await
        {
            Ok(compiled) => {
                render_engine.set_cue_buffer(deck_id, &cue.id, compiled);
            }
            Err(e) => {
                eprintln!(
                    "[controller_compositor] cue={} deck={} error: {}",
                    cue.id, deck_id, e
                );
            }
        }
    }

    Ok(())
}

/// Compile all cues for the always-running simulated deck (deck_id=99).
/// Uses a synthetic 120 BPM / 4/4 beat grid and 30s duration — no real track or audio.
/// Audio-reactive cues compile with no audio context and produce flat output, which is
/// correct for when no music is playing.
pub async fn compile_cues_for_simulated_deck(
    pool: &SqlitePool,
    stem_cache: &StemCache,
    fft_service: &FftService,
    resource_path_root: Option<std::path::PathBuf>,
    render_engine: &RenderEngine,
    venue_id: &str,
) -> Result<(), String> {
    let cues = crate::database::local::midi::list_cues(pool, venue_id).await?;
    if cues.is_empty() {
        return Ok(());
    }

    let beat_grid = Some(synthetic_beat_grid(
        SIM_BPM,
        SIM_BEATS_PER_BAR,
        SIM_DURATION,
    ));

    for cue in &cues {
        match compile_single_cue(
            pool,
            stem_cache,
            fft_service,
            resource_path_root.clone(),
            cue,
            "simulated",
            venue_id,
            &beat_grid,
            SIM_BPM,
            SIM_BEATS_PER_BAR,
            SIM_DURATION,
            None, // no shared audio
        )
        .await
        {
            Ok(compiled) => {
                render_engine.set_cue_buffer(SIM_DECK_ID, &cue.id, compiled);
            }
            Err(e) => {
                eprintln!(
                    "[controller_compositor] sim deck cue={} error: {}",
                    cue.id, e
                );
            }
        }
    }

    Ok(())
}

async fn compile_single_cue(
    pool: &SqlitePool,
    stem_cache: &StemCache,
    fft_service: &FftService,
    resource_path_root: Option<std::path::PathBuf>,
    cue: &Cue,
    track_id: &str,
    venue_id: &str,
    beat_grid: &Option<BeatGrid>,
    bpm: f32,
    beats_per_bar: i32,
    track_duration: f32,
    shared_audio: Option<crate::node_graph::SharedAudioContext>,
) -> Result<CompiledCue, String> {
    let graph_json = fetch_pattern_graph(pool, &cue.pattern_id).await?;
    let graph: Graph = serde_json::from_str(&graph_json)
        .map_err(|e| format!("Failed to parse pattern graph for cue {}: {}", cue.id, e))?;

    if graph.nodes.is_empty() {
        return Err(format!("Cue {} has empty graph", cue.id));
    }

    let effective_mode = resolve_execution_mode(&cue.execution_mode, &graph);

    // Always compile for full track duration — patterns repeat naturally in the buffer.
    let start_time = 0.0f32;
    let end_time = track_duration;

    let arg_values: HashMap<String, serde_json::Value> = cue
        .args
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();

    let context = GraphContext {
        track_id: track_id.to_string(),
        venue_id: venue_id.to_string(),
        start_time,
        end_time,
        beat_grid: beat_grid.clone(),
        arg_values: Some(arg_values),
        instance_seed: Some(rand::random::<u64>()),
    };

    let config = GraphExecutionConfig {
        compute_visualizations: false,
        log_summary: false,
        log_primitives: false,
        shared_audio,
    };

    let (_result, layer_opt) = run_graph_internal(
        pool,
        Some(pool),
        stem_cache,
        fft_service,
        resource_path_root,
        graph,
        context,
        config,
    )
    .await?;

    let layer = layer_opt.ok_or_else(|| format!("Cue {} produced no layer", cue.id))?;

    Ok(CompiledCue {
        layer,
        execution_mode: effective_mode,
        z_index: cue.z_index as i8,
        blend_mode: cue.blend_mode,
    })
}

/// Determine execution mode — still useful to know whether audio data was used.
fn resolve_execution_mode(declared: &CueExecutionMode, graph: &Graph) -> CompiledCueMode {
    match declared {
        CueExecutionMode::TrackTime => CompiledCueMode::TrackTime,
        CueExecutionMode::Loop { .. } => {
            if graph_requires_track_time(graph) {
                CompiledCueMode::TrackTime
            } else {
                CompiledCueMode::Loop
            }
        }
    }
}

/// Build a synthetic beat grid at a fixed BPM for the simulated deck.
pub fn synthetic_beat_grid(bpm: f32, beats_per_bar: i32, duration: f32) -> BeatGrid {
    let beat_interval = 60.0 / bpm;
    let beats: Vec<f32> = (0..)
        .map(|i| i as f32 * beat_interval)
        .take_while(|&t| t < duration)
        .collect();
    let downbeats: Vec<f32> = beats
        .iter()
        .copied()
        .enumerate()
        .filter(|(i, _)| i % beats_per_bar as usize == 0)
        .map(|(_, t)| t)
        .collect();
    BeatGrid {
        beats,
        downbeats,
        bpm,
        downbeat_offset: 0.0,
        beats_per_bar,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_requires_track_time_detects_audio_nodes() {
        use crate::models::node_graph::{Graph, NodeInstance};
        use std::collections::HashMap;

        let make_graph = |type_id: &str| Graph {
            nodes: vec![NodeInstance {
                id: "n1".into(),
                type_id: type_id.into(),
                params: HashMap::new(),
                position_x: None,
                position_y: None,
            }],
            edges: vec![],
            args: vec![],
        };

        assert!(graph_requires_track_time(&make_graph("harmony_analysis")));
        assert!(graph_requires_track_time(&make_graph("audio_input")));
        assert!(graph_requires_track_time(&make_graph(
            "frequency_amplitude"
        )));
        assert!(graph_requires_track_time(&make_graph("beat_input")));
        assert!(!graph_requires_track_time(&make_graph("color_constant")));
        assert!(!graph_requires_track_time(&make_graph("strobe")));
    }

    #[test]
    fn resolve_execution_mode_tracktime_declared() {
        use crate::models::node_graph::Graph;

        let graph = Graph {
            nodes: vec![],
            edges: vec![],
            args: vec![],
        };
        // Explicit TrackTime stays TrackTime even with no audio nodes
        assert!(matches!(
            resolve_execution_mode(&CueExecutionMode::TrackTime, &graph),
            CompiledCueMode::TrackTime
        ));
    }

    #[test]
    fn resolve_execution_mode_loop_upgrades_when_audio_nodes_present() {
        use crate::models::node_graph::{Graph, NodeInstance};
        use std::collections::HashMap;

        let graph = Graph {
            nodes: vec![NodeInstance {
                id: "n1".into(),
                type_id: "beat_input".into(),
                params: HashMap::new(),
                position_x: None,
                position_y: None,
            }],
            edges: vec![],
            args: vec![],
        };
        assert!(matches!(
            resolve_execution_mode(&CueExecutionMode::Loop { bars: 4 }, &graph),
            CompiledCueMode::TrackTime
        ));
    }

    #[test]
    fn resolve_execution_mode_loop_stays_loop_without_audio_nodes() {
        use crate::models::node_graph::Graph;

        let graph = Graph {
            nodes: vec![],
            edges: vec![],
            args: vec![],
        };
        assert!(matches!(
            resolve_execution_mode(&CueExecutionMode::Loop { bars: 4 }, &graph),
            CompiledCueMode::Loop
        ));
    }

    #[test]
    fn synthetic_beat_grid_correct_count() {
        let grid = synthetic_beat_grid(120.0, 4, 30.0);
        // 120 BPM = 2 beats/sec → 60 beats in 30s
        assert_eq!(grid.beats.len(), 60);
        // 60 beats / 4 beats_per_bar = 15 downbeats
        assert_eq!(grid.downbeats.len(), 15);
        assert_eq!(grid.bpm, 120.0);
        assert_eq!(grid.beats_per_bar, 4);
    }

    #[test]
    fn synthetic_beat_grid_timing_accuracy() {
        let grid = synthetic_beat_grid(120.0, 4, 30.0);
        // Beat 0 starts at t=0
        assert!((grid.beats[0] - 0.0).abs() < 1e-5);
        // Beat 1 at t=0.5s (120 BPM = 0.5s/beat)
        assert!((grid.beats[1] - 0.5).abs() < 1e-5);
        // First downbeat at t=0
        assert!((grid.downbeats[0] - 0.0).abs() < 1e-5);
        // Second downbeat at t=2.0s (4 beats × 0.5s)
        assert!((grid.downbeats[1] - 2.0).abs() < 1e-5);
        // Last beat < 30s
        assert!(*grid.beats.last().unwrap() < 30.0);
    }
}
