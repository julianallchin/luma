//! Host boundary for the shared tensor evaluator: fixture output and scene compositing.
pub mod composite;
pub mod context;
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
/// One prepared canonical graph and its host selection.
#[derive(Clone, Debug)]
pub struct Plan {
    pub program: Option<Arc<lighting::Program>>,
    pub primitive_ids: Vec<String>,
    pub outputs: OutputBinding,
    /// The clip's absolute `[start, end]` time span in seconds.
    pub span: (f32, f32),
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
        Some(program) => program.views(times, &plan.views, plan.span, scratch),
        None => Ok(HashMap::new()),
    }
}
