//! Host boundary for the shared tensor evaluator: fixture output and scene compositing.
pub mod aim;
pub mod composite;
pub mod context;
pub mod lighting;
pub mod scene;
pub(crate) mod track_features;
pub use crate::models::node_graph::BlendMode;
use crate::models::universe::UniverseState;
pub use scene::{CompiledAnnotation, Scene, Scope};
use std::{collections::BTreeMap, sync::Arc};

/// A track's mono audio at [`crate::audio::SAMPLE_RATE`], shared.
pub type ResidentAudio = Arc<Vec<f32>>;

/// Capabilities authored by a graph. Unwritten channels preserve lower layers.
#[derive(Clone, Debug, Default)]
pub struct OutputBinding {
    pub dimmer: bool,
    pub color: bool,
    pub position: bool,
    pub strobe: bool,
    pub speed: bool,
    pub aim: bool,
}
/// One prepared canonical graph and its host selection.
#[derive(Clone, Debug)]
pub struct Plan {
    pub program: Option<Arc<lighting::Program>>,
    pub primitive_ids: Vec<String>,
    pub outputs: OutputBinding,
    /// The clip's absolute `[start, end]` time span in seconds.
    pub span: (f32, f32),
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
/// One clip alone over no light: its light and strobe at its opacity.
pub fn try_eval(
    plan: &Plan,
    times: &[f32],
    scratch: &mut Arena,
) -> Result<Vec<UniverseState>, String> {
    Ok(try_layers(plan, times, scratch)?
        .into_iter()
        .map(lighting::Layer::over_nothing)
        .collect())
}

/// One clip's frames at `times` with its opacity per head, for compositing.
pub(crate) fn try_layers(
    plan: &Plan,
    times: &[f32],
    scratch: &mut Arena,
) -> Result<Vec<lighting::Layer>, String> {
    match &plan.program {
        Some(program) => program.layers(times, &plan.outputs, scratch),
        None => Ok(times
            .iter()
            .map(|_| lighting::Layer {
                frame: composite::blank_frame(),
                alpha: Default::default(),
            })
            .collect()),
    }
}

/// Each head's aim turn at `times`, for a plan whose clip blends with Offset.
pub(crate) fn try_turns(
    plan: &Plan,
    times: &[f32],
    scratch: &mut Arena,
) -> Result<Vec<BTreeMap<String, luma_patterns::Turn>>, String> {
    match &plan.program {
        Some(program) => program.turns(times, scratch),
        None => Ok(times.iter().map(|_| BTreeMap::new()).collect()),
    }
}
