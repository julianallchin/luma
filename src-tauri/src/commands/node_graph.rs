use tauri::{AppHandle, State};

use crate::audio::{FftService, StemCache};
use crate::database::Db;
use crate::host_audio::HostAudioState;
use crate::models::node_graph::{Graph, GraphContext, NodeTypeDef, RunResult};

#[tauri::command]
pub fn get_node_types() -> Vec<NodeTypeDef> {
    crate::node_graph::nodes::get_node_types()
}

#[tauri::command]
pub async fn run_graph(
    app: AppHandle,
    db: State<'_, Db>,
    host_audio: State<'_, HostAudioState>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    graph: Graph,
    context: GraphContext,
) -> Result<RunResult, String> {
    crate::node_graph::run_graph(app, db, host_audio, stem_cache, fft_service, graph, context).await
}
