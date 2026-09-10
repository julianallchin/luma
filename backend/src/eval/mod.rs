//! Host boundary for the shared tensor evaluator: fixture output and scene compositing.
pub mod compile;
pub mod composite;
pub mod context;
pub mod graph_run;
pub mod lighting;
pub mod scene;
pub(crate) mod track_features;
pub use crate::models::node_graph::BlendMode;
use crate::models::universe::UniverseState;
pub use scene::{CompiledAnnotation, Scene, Scope};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

#[derive(Clone, Debug, Default)]
pub struct ResidentAudio {
    pub samples: std::sync::Arc<Vec<f32>>,
    pub sample_rate: u32,
}

/// Immutable host data provided when importing a saved standalone graph.
#[derive(Clone, Debug, Default)]
pub struct ResidentContext {
    /// Stable authored clip seed for composable graph randomness.
    pub seed: u64,
    /// Per-primitive world position `[x, y, z]`, length `n` (spatial ops).
    pub positions: Vec<[f32; 3]>,
    /// Beat grid for beat-synced generators.
    pub beat_grid: Option<crate::models::node_graph::BeatGrid>,
    /// Resident decoded audio for audio ops.
    pub audio: Option<ResidentAudio>,
    /// Per-stem decoded audio (key = `drums|bass|vocals|other`), same timeline as
    /// `audio`. Consumed by `StemSplit`; populated by the compiler from the stem cache.
    pub stems: std::collections::HashMap<String, ResidentAudio>,
    /// Drum-onset times per class (`kick|snare|hat|cymbal`), from the track's
    /// detected onsets, in absolute track seconds.
    pub drum_onsets: std::collections::HashMap<String, Vec<f32>>,
    /// Detected chord sections `(start, end, root_pitch_class)` over absolute time
    /// (`root` is `0..11`, `None` = no chord). Consumed by `harmony_analysis`,
    /// which emits a one-hot 12-channel chroma signal per frame.
    pub chord_sections: Vec<(f32, f32, Option<u8>)>,
    /// The annotation's absolute `[start, end]` time span. Span-relative temporal
    /// ops (`ramp_between`) compute progress as
    /// `(t - start)/(end - start)` using the shared absolute clock.
    pub span: (f32, f32),
}

/// Capabilities authored by a graph. Unwritten channels preserve lower layers.
#[derive(Clone, Debug, Default)]
pub struct OutputBinding {
    pub dimmer: bool,
    pub color: bool,
    pub position: bool,
    pub strobe: bool,
    pub speed: bool,
}
#[derive(Clone, Debug)]
pub struct ViewTap {
    pub output: String,
    pub channels: Vec<String>,
    pub n: usize,
    pub c: usize,
}
/// One prepared canonical graph and its host selection/context.
#[derive(Clone, Debug)]
pub struct Plan {
    pub program: Option<Arc<lighting::Program>>,
    pub primitive_ids: Vec<String>,
    pub outputs: OutputBinding,
    pub ctx: ResidentContext,
    pub views: Vec<(String, ViewTap)>,
}
impl Plan {
    pub fn view_channels(&self, tap: &ViewTap) -> Vec<String> {
        tap.channels.clone()
    }
}
/// Last batch of canonical values, reusable by output assembly and diagnostics.
#[derive(Default)]
pub struct Arena {
    pub(crate) values: BTreeMap<String, luma_patterns::EvaluatedValue>,
}
pub fn eval(plan: &Plan, times: &[f32], scratch: &mut Arena) -> Vec<UniverseState> {
    try_eval(plan, times, scratch).unwrap_or_else(|error| {
        log::error!("Graph evaluation: {error}");
        times.iter().map(|_| composite::blank_frame()).collect()
    })
}
pub fn try_eval(
    plan: &Plan,
    times: &[f32],
    scratch: &mut Arena,
) -> Result<Vec<UniverseState>, String> {
    match &plan.program {
        Some(program) => program.render(times, &plan.outputs, scratch),
        None => Ok(times.iter().map(|_| composite::blank_frame()).collect()),
    }
}
pub fn eval_views(
    plan: &Plan,
    times: &[f32],
    scratch: &mut Arena,
) -> Result<HashMap<String, crate::models::node_graph::Signal>, String> {
    match &plan.program {
        Some(program) => program.views(times, &plan.views, plan.ctx.span, scratch),
        None => Ok(HashMap::new()),
    }
}
