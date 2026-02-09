use tauri::{AppHandle, State};

use crate::audio::{FftService, StemCache};
use crate::database::Db;
use crate::models::node_graph::{Graph, GraphContext, NodeTypeDef, RunResult};
use crate::render_engine::RenderEngine;

#[tauri::command]
pub fn get_node_types() -> Vec<NodeTypeDef> {
    crate::node_graph::nodes::get_node_types()
}

#[tauri::command]
pub async fn run_graph(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    graph: Graph,
    context: GraphContext,
) -> Result<RunResult, String> {
    crate::node_graph::run_graph(
        app,
        db,
        render_engine,
        stem_cache,
        fft_service,
        graph,
        context,
    )
    .await
}
