use crate::agent_execution::graph_runs::authorize_publish_target;
use crate::database::local::auth;
use crate::database::local::venue_access::{Operate, Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::eval::compile::compile_pattern;
use crate::eval::context::build_resident_context;
use crate::eval::graph_run::{evaluate_graph, merge_arg_values, EvaluateOptions};
use crate::eval::{Arena, CompiledAnnotation, Scene};
use crate::models::node_graph::{BeatGrid, BlendMode, Graph, GraphContext, NodeTypeDef, RunResult};
use crate::models::universe::UniverseState;

/// The node-type catalogue is a pure in-memory read; it takes no services at
/// all. Handlers stay uniform in shape anyway so the registry can generate both
/// adapters without special cases.
pub async fn get_node_types(_services: &AppServices) -> Result<Vec<NodeTypeDef>, CommandError> {
    Ok(crate::node_graph::nodes::get_node_types())
}

/// Compile a graph against the eval engine, install it as the active scene for
/// live visualization, and return the editor's `RunResult`.
///
/// The evaluation itself lives in [`evaluate_graph`], which produces strictly
/// more than `RunResult` can carry (the time grid, primitive ids/positions,
/// channel names, hashes); this command projects it down to the wire shape the
/// graph editor has always consumed.
///
/// `include_mel_specs` is false for param-only edits (live slider drags): mel
/// specs depend only on audio wiring + span, so their FFT and heavy payload can
/// be skipped.
///
/// `agent_thread_id` parks the full evaluation under an agent conversation so
/// its next Python cell can bind `luma.graph.run`. That association is a publish
/// target, not part of the semantic `GraphContext`. `agent_execution_id` is an
/// optional child namespace for Python workspace/run storage only —
/// authorization stays pinned to the durable parent thread. Detached subagent
/// runs pass `drive_live_preview: false` to evaluate and publish without
/// replacing the user's live scene.
#[allow(clippy::too_many_arguments)]
pub async fn run_graph(
    services: &AppServices,
    graph: Graph,
    context: GraphContext,
    include_mel_specs: Option<bool>,
    agent_thread_id: Option<String>,
    agent_execution_id: Option<String>,
    drive_live_preview: Option<bool>,
) -> Result<RunResult, CommandError> {
    let pool = &services.db.0;
    let venue_access =
        VenueAccess::<Read>::read(pool, VenueResource::Venue(&context.venue_id)).await?;
    let admitted_principal = venue_access.principal().map(str::to_owned);
    drop(venue_access);
    let owner_user_id = if let Some(thread_id) = agent_thread_id.as_deref() {
        let owner_user_id = auth::admitted_principal(pool).await?;
        authorize_publish_target(pool, thread_id, owner_user_id.as_deref()).await?;
        if let Some(execution_id) = agent_execution_id.as_deref() {
            services
                .authored
                .authorize_workspace(pool, owner_user_id.as_deref(), thread_id, execution_id)
                .await?;
        }
        owner_user_id
    } else {
        None
    };

    let evaluation = evaluate_graph(
        pool,
        &services.storage,
        &services.fixtures_root,
        &services.fft,
        &graph,
        &context,
        EvaluateOptions {
            include_mel: include_mel_specs.unwrap_or(true),
        },
    )
    .await?;

    // Drive live visualization from the run. An empty graph clears the scene
    // instead of installing a plan that outputs nothing.
    let drive_live_preview = drive_live_preview.unwrap_or(true);
    let scene = (!graph.nodes.is_empty()).then(|| {
        Scene::new(vec![CompiledAnnotation {
            plan: evaluation.plan.clone(),
            span: evaluation.span,
            z_index: 0,
            blend_mode: BlendMode::Replace,
        }])
    });
    let final_access =
        VenueAccess::<Operate>::operate(pool, VenueResource::Venue(&context.venue_id)).await?;
    if final_access.principal() != admitted_principal.as_deref() {
        return Err(CommandError::Unauthorized(
            "authenticated identity changed while running graph".into(),
        ));
    }
    if let Some(thread_id) = agent_thread_id {
        let execution_id = agent_execution_id.as_deref().unwrap_or(&thread_id);
        services
            .graph_runs
            .commit_evaluation(
                pool,
                &services.authored,
                &thread_id,
                owner_user_id.as_deref(),
                execution_id,
                std::sync::Arc::new(evaluation.clone()),
                || {
                    if drive_live_preview {
                        services.render_engine.set_active_scene(scene);
                    }
                },
            )
            .await?;
    } else if drive_live_preview {
        services.render_engine.set_active_scene(scene);
    }
    final_access.commit().await?;

    Ok(evaluation.into_run_result())
}

/// Precompute a looping pattern preview as a sequence of `UniverseState` frames.
/// Used by the hover-card preview to play back smoothly without per-frame IPC.
#[allow(clippy::too_many_arguments)]
pub async fn preview_pattern(
    services: &AppServices,
    pattern_id: String,
    track_id: String,
    venue_id: String,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
    fps: f32,
) -> Result<Vec<UniverseState>, CommandError> {
    use crate::compositor::fetch_pattern_graph;

    let pool = &services.db.0;
    let duration = end_time - start_time;
    if duration <= 0.0 {
        return Err(CommandError::Invalid(
            "Preview duration must be positive".into(),
        ));
    }

    let fps = fps.clamp(10.0, 30.0);
    let frame_count = ((duration * fps).ceil() as usize).clamp(1, 256);
    let venue_access = VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
    let admitted_principal = venue_access.principal().map(str::to_owned);
    drop(venue_access);

    let graph_json = fetch_pattern_graph(pool, &pattern_id, Some(&venue_id)).await?;
    let graph: Graph = serde_json::from_str(&graph_json).map_err(|error| {
        CommandError::Internal(format!("Failed to parse pattern graph: {error}"))
    })?;
    if graph.nodes.is_empty() {
        return Err(CommandError::Invalid("Pattern produced no output".into()));
    }

    // Args from the pattern's defaults, with Selection forced to "all" — the
    // hover card has no venue selection context.
    let args = merge_arg_values(&graph, None, true);

    let span = (start_time, end_time);
    let (ctx, primitive_ids) = build_resident_context(
        pool,
        pool,
        &services.storage,
        &services.fixtures_root,
        &track_id,
        &venue_id,
        &graph.nodes,
        &graph.edges,
        &args,
        span,
        beat_grid,
    )
    .await;

    let plan = compile_pattern(&graph.nodes, &graph.edges, &args, ctx, primitive_ids)
        .map_err(|error| CommandError::Internal(format!("Failed to compile pattern: {error:?}")))?;

    let dt = duration / frame_count as f32;
    let times: Vec<f32> = (0..frame_count)
        .map(|i| start_time + i as f32 * dt)
        .collect();
    let mut arena = Arena::default();
    let frames = crate::eval::eval(&plan, &times, &mut arena);
    let final_access =
        VenueAccess::<Operate>::operate(pool, VenueResource::Venue(&venue_id)).await?;
    if final_access.principal() != admitted_principal.as_deref() {
        return Err(CommandError::Unauthorized(
            "authenticated identity changed while rendering preview".into(),
        ));
    }
    final_access.commit().await?;
    Ok(frames)
}
