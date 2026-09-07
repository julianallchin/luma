//! `luma.graph` — the pattern graph the agent is editing, and its latest run.
//!
//! Two halves with two different owners. The **definition** is whatever is in
//! the editor right now, including unsaved edits, so it can only come from the
//! frontend; it is inlined verbatim-ish (nodes, edges, args). The **run** is
//! dense numeric output the frontend has no business shipping, so it is
//! published in Rust from a [`GraphEvaluation`].
//!
//! The run is only published when it still describes the current scope (design
//! §11.3). A stale run paired with a fresh track is worse than no run at all:
//! the agent would draw conclusions about audio the tensors never saw. So the
//! provider re-derives the graph hash from the scope's own definition and
//! compares — it does not trust the caller's claim that the run is current.

use std::sync::Arc;

use serde::Serialize;

use super::{inline, put_f32, unavailable, ProviderCtx};
use crate::agent_execution::artifacts::ArtifactStore;
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::agent_execution::bindings::manifest::{AxisSpec, Provenance};
use crate::eval::graph_run::{graph_hash, GraphEvaluation, SemanticMel, SemanticSignal};
use crate::models::node_graph::Graph;

/// Spans agree to within a millisecond — they are f32 seconds that survived a
/// round trip through JSON.
const SPAN_EPSILON: f32 = 1e-3;

pub const NO_RUN: &str = "no graph run has been evaluated for this thread yet";
pub const GRAPH_CHANGED: &str = "the graph has changed since its latest run";
pub const NO_DEFINITION: &str =
    "the graph editor has not published a graph definition to this thread";

/// The caller's latest evaluation, offered to the assembler. Wrapped rather than
/// passed bare so the compatibility contract has somewhere to live.
#[derive(Clone)]
pub struct GraphRunContribution {
    pub evaluation: Arc<GraphEvaluation>,
}

impl GraphRunContribution {
    pub fn new(evaluation: Arc<GraphEvaluation>) -> Self {
        Self { evaluation }
    }

    /// `None` when the run still describes `scope`; otherwise the reason it does
    /// not, phrased for the agent.
    pub fn incompatibility(&self, scope: &super::BindingScope) -> Option<String> {
        let evaluation = &self.evaluation;
        if scope.track_id.as_deref() != Some(evaluation.track_id.as_str()) {
            return Some(format!(
                "the latest graph run is for a different track ({})",
                evaluation.track_id
            ));
        }
        if scope.venue_id.as_deref() != Some(evaluation.venue_id.as_str()) {
            return Some(format!(
                "the latest graph run is for a different venue ({})",
                evaluation.venue_id
            ));
        }
        if let Some((start_s, end_s)) = scope.window {
            let (run_start, run_end) = evaluation.span;
            if (run_start - start_s as f32).abs() > SPAN_EPSILON
                || (run_end - end_s as f32).abs() > SPAN_EPSILON
            {
                return Some(format!(
                    "the latest graph run covers {run_start:.3}s-{run_end:.3}s, \
                     not the window in scope"
                ));
            }
        }
        match scope.graph_definition.as_ref() {
            None => None,
            Some(definition) => match serde_json::from_value::<Graph>(definition.clone()) {
                Ok(graph) if graph_hash(&graph) == evaluation.graph_hash => None,
                Ok(_) => Some(GRAPH_CHANGED.to_string()),
                Err(e) => Some(format!(
                    "the graph definition in scope could not be parsed, so the latest run \
                     cannot be shown to match it: {e}"
                )),
            },
        }
    }
}

#[derive(Serialize)]
struct NodeBinding {
    id: String,
    #[serde(rename = "type")]
    type_id: String,
    params: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct EdgeBinding {
    id: String,
    from_node: String,
    from_port: String,
    to_node: String,
    to_port: String,
}

pub fn provide(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
    contribution: Option<&GraphRunContribution>,
) -> Result<(), String> {
    definition(b, ctx)?;

    let Some(contribution) = contribution else {
        return unavailable(b, "graph.run", NO_RUN);
    };
    if let Some(reason) = contribution.incompatibility(ctx.scope) {
        return unavailable(b, "graph.run", reason);
    }
    run(b, store, &contribution.evaluation)
}

fn definition(b: &mut BindingBuilder, ctx: &ProviderCtx<'_>) -> Result<(), String> {
    let Some(value) = ctx.scope.graph_definition.as_ref() else {
        return unavailable(b, "graph.definition", NO_DEFINITION);
    };
    let graph: Graph = match serde_json::from_value(value.clone()) {
        Ok(g) => g,
        Err(e) => {
            return unavailable(
                b,
                "graph.definition",
                format!("the graph definition in scope is not a valid graph: {e}"),
            )
        }
    };

    let nodes: Vec<NodeBinding> = graph
        .nodes
        .iter()
        .map(|n| NodeBinding {
            id: n.id.clone(),
            type_id: n.type_id.clone(),
            params: n.params.clone().into_iter().collect(),
        })
        .collect();
    let edges: Vec<EdgeBinding> = graph
        .edges
        .iter()
        .map(|e| EdgeBinding {
            id: e.id.clone(),
            from_node: e.from_node.clone(),
            from_port: e.from_port.clone(),
            to_node: e.to_node.clone(),
            to_port: e.to_port.clone(),
        })
        .collect();

    inline(b, "graph.definition.nodes", &nodes)?;
    inline(b, "graph.definition.edges", &edges)?;
    inline(b, "graph.definition.args", &graph.args)
}

fn run(
    b: &mut BindingBuilder,
    store: &mut ArtifactStore,
    evaluation: &GraphEvaluation,
) -> Result<(), String> {
    let ids = &evaluation.primitive_ids;

    // Deterministic order: `views` is a HashMap, and the manifest must not
    // depend on hash iteration order.
    let mut views: Vec<(&String, &SemanticSignal)> = evaluation
        .views
        .iter()
        .filter(|(id, _)| super::features::is_safe_record_key(id))
        .collect();
    views.sort_by(|a, b| a.0.cmp(b.0));
    if views.is_empty() {
        empty_record(b, "graph.run.views")?;
    }
    for (node_id, view) in views {
        bind_view(b, store, node_id, view, ids, &evaluation.times_s)?;
    }

    match evaluation.mel_views.as_ref() {
        None => unavailable(
            b,
            "graph.run.mel_views",
            "mel spectrograms were not computed for this run",
        )?,
        Some(mels) => {
            let mut mels: Vec<(&String, &SemanticMel)> = mels
                .iter()
                .filter(|(id, _)| super::features::is_safe_record_key(id))
                .collect();
            mels.sort_by(|a, b| a.0.cmp(b.0));
            if mels.is_empty() {
                empty_record(b, "graph.run.mel_views")?;
            }
            for (node_id, mel) in mels {
                bind_mel(b, store, node_id, mel)?;
            }
        }
    }

    inline(b, "graph.run.primitive_ids", ids)?;
    let positions: Vec<f32> = evaluation.positions.iter().flat_map(|p| *p).collect();
    if positions.len() == ids.len() * 3 {
        put_f32(
            b,
            store,
            "graph.run.positions",
            &positions,
            vec![
                AxisSpec::labels("primitive", ids.clone()),
                AxisSpec::labels("coordinate", vec!["x".into(), "y".into(), "z".into()]),
            ],
            Some("m"),
            Provenance::new("graph_run").with_note("world positions of the run's own primitives"),
        )?;
    } else {
        unavailable(
            b,
            "graph.run.positions",
            "the run's positions and primitive ids disagree in length",
        )?;
    }

    inline(b, "graph.run.span.start_s", evaluation.span.0)?;
    inline(b, "graph.run.span.end_s", evaluation.span.1)?;
    inline(b, "graph.run.fingerprints.graph", &evaluation.graph_hash)?;
    inline(b, "graph.run.fingerprints.args", &evaluation.arg_hash)?;
    inline(
        b,
        "graph.run.fingerprints.selection",
        &evaluation.selection_hash,
    )?;
    inline(b, "graph.run.track_id", &evaluation.track_id)?;
    inline(b, "graph.run.venue_id", &evaluation.venue_id)
}

/// An empty namespace node — "the run has none of these", as distinct from
/// "this branch is unavailable".
fn empty_record(b: &mut BindingBuilder, path: &str) -> Result<(), String> {
    b.record(
        path,
        crate::agent_execution::bindings::manifest::BindingValue::record(),
    )
    .map_err(String::from)?;
    Ok(())
}

/// One view tap as `[primitive, time, channel]`. `Signal.data` is already
/// `[n][t][c]` row-major, so it is written straight through.
fn bind_view(
    b: &mut BindingBuilder,
    store: &mut ArtifactStore,
    node_id: &str,
    view: &SemanticSignal,
    primitive_ids: &[String],
    times_s: &[f32],
) -> Result<(), String> {
    let signal = &view.signal;
    let (n, t, c) = (signal.n, signal.t, signal.c);
    let path = format!("graph.run.views.{node_id}");
    if signal.data.len() != n * t * c {
        return unavailable(
            b,
            &path,
            format!(
                "the run produced {} values for a [{n}, {t}, {c}] view",
                signal.data.len()
            ),
        );
    }

    // A tap whose slot is broadcast over primitives has n == 1: it is not
    // indexed by the run's primitives, and labeling it as if it were would be a
    // lie about identity (§8.5).
    let primitive_axis = if n == primitive_ids.len() {
        AxisSpec::labels("primitive", primitive_ids.to_vec())
    } else {
        AxisSpec::index("primitive", n)
    };
    let channel_axis = if view.channels.len() == c {
        AxisSpec::labels("channel", view.channels.clone())
    } else {
        AxisSpec::index("channel", c)
    };

    put_f32(
        b,
        store,
        &path,
        &signal.data,
        vec![primitive_axis, time_axis(times_s, t), channel_axis],
        None,
        Provenance::new("graph_run").with_note(format!("view tap '{node_id}'")),
    )
}

/// The run's own sampling grid: uniform over the span and hitting both
/// endpoints, so it is exactly a linear axis. Falls back to a bare index when a
/// tap somehow disagrees with the grid it was sampled on.
fn time_axis(times_s: &[f32], t: usize) -> AxisSpec {
    if times_s.len() != t || t < 2 {
        return AxisSpec::index("time", t);
    }
    let start = times_s[0] as f64;
    let step = (times_s[t - 1] as f64 - start) / (t - 1) as f64;
    AxisSpec::linear_unit("time", start, step, t, "s")
}

/// A mel viewer's spectrogram as `[time, frequency]` — `MelSpec.data` is stored
/// column-major (`data[col * height + bin]`), which is row-major `[time, freq]`.
fn bind_mel(
    b: &mut BindingBuilder,
    store: &mut ArtifactStore,
    node_id: &str,
    mel: &SemanticMel,
) -> Result<(), String> {
    let path = format!("graph.run.mel_views.{node_id}");
    let (w, h) = (mel.mel.width, mel.mel.height);
    if mel.mel.data.len() != w * h || mel.times_s.len() != w || mel.frequencies_hz.len() != h {
        return unavailable(
            b,
            &path,
            "the run's mel spectrogram has inconsistent dimensions",
        );
    }
    put_f32(
        b,
        store,
        &path,
        &mel.mel.data,
        vec![
            AxisSpec::coordinates(
                "time",
                mel.times_s.iter().map(|v| *v as f64).collect(),
                Some("s".into()),
            ),
            AxisSpec::coordinates(
                "frequency",
                mel.frequencies_hz.iter().map(|v| *v as f64).collect(),
                Some("Hz".into()),
            ),
        ],
        None,
        Provenance::new("graph_run").with_note(
            "mel viewer output: log-scaled then min-max normalized to 0-1 over the whole \
             spectrogram — not dB and not absolute",
        ),
    )
}
