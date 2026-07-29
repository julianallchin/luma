//! Domain binding providers — the layer that turns Luma's databases and caches
//! into one [`BindingManifest`].
//!
//! Each provider owns one branch of the `luma` namespace and knows nothing about
//! Python; the manifest is the only contract. They all write into a single
//! [`BindingBuilder`], which owns the cross-provider invariants (no duplicate
//! paths, every tensor consistent with its artifact).
//!
//! **A missing data source is not an error.** A provider that cannot load its
//! branch marks that *path* unavailable with the honest reason (design §9.7) and
//! keeps going; only a broken builder invariant or a failed workspace write
//! aborts the assembly. That distinction is the whole point: the agent must be
//! able to tell "this track has no stems" from "the assembly blew up".
//!
//! No Tauri types appear here. `resource_root` (the bundled fixture-definition
//! root) is passed as a plain `&Path` so the headless harness can assemble
//! bindings exactly like the app does.

pub mod audio;
pub mod features;
pub mod graph;
pub mod patterns;
pub mod score;
pub mod track;
pub mod venue;

use std::path::Path;

use serde_json::Value;
use sqlx::SqlitePool;

use crate::agent_execution::artifacts::ArtifactStore;
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::agent_execution::bindings::manifest::{
    AgentKind, AnalysisScope, AnalysisWindow, AxisSpec, BindingManifest, DType, Provenance,
    TensorRef,
};
use crate::models::tracks::TrackSummary;
use crate::storage::StorageRoot;

pub use graph::GraphRunContribution;

/// Why a track-derived branch is missing when the thread has no track at all.
pub const NO_TRACK: &str = "no track is in scope for this agent thread";
/// Why a venue-derived branch is missing when the thread has no venue.
pub const NO_VENUE: &str = "no venue is in scope for this agent thread";

/// What the agent is looking at, resolved by the command/adapter layer.
///
/// Deliberately free of Tauri types and of anything the frontend cannot supply:
/// `graph_definition` is the editor's *unsaved* buffer, the one piece of scope
/// that only the frontend knows.
#[derive(Debug, Clone, Default)]
pub struct BindingScope {
    /// `"track_copilot"` | `"pattern_graph"`.
    pub agent_kind: String,
    pub track_id: Option<String>,
    pub venue_id: Option<String>,
    pub score_id: Option<String>,
    pub pattern_id: Option<String>,
    /// Window of interest in absolute track seconds.
    pub window: Option<(f64, f64)>,
    /// The frontend-owned graph currently in the editor (Graph-shaped JSON).
    pub graph_definition: Option<Value>,
}

impl BindingScope {
    fn agent_kind(&self) -> Result<AgentKind, String> {
        match self.agent_kind.as_str() {
            "track_copilot" => Ok(AgentKind::TrackCopilot),
            "pattern_graph" => Ok(AgentKind::PatternGraph),
            other => Err(format!("unknown agent kind '{other}'")),
        }
    }

    fn analysis_scope(&self) -> AnalysisScope {
        AnalysisScope {
            track_id: self.track_id.clone(),
            venue_id: self.venue_id.clone(),
            score_id: self.score_id.clone(),
            pattern_id: self.pattern_id.clone(),
            window: self
                .window
                .map(|(start_s, end_s)| AnalysisWindow { start_s, end_s }),
        }
    }
}

/// Everything the providers share. Read-only; the artifact store is threaded
/// separately because it is the one mutable resource.
pub struct ProviderCtx<'a> {
    pub pool: &'a SqlitePool,
    pub storage: &'a StorageRoot,
    /// Root of the bundled QLC+ fixture definitions — needed to expand a
    /// fixture into its heads, i.e. into evaluator primitives.
    pub resource_root: &'a Path,
    pub scope: &'a BindingScope,
    /// Resolved once; every track-derived provider needs the hash.
    pub track: Option<TrackSummary>,
}

impl ProviderCtx<'_> {
    pub fn track_hash(&self) -> Option<&str> {
        self.track.as_ref().map(|t| t.track_hash.as_str())
    }

    pub fn track_id(&self) -> Option<&str> {
        self.scope.track_id.as_deref()
    }
}

/// Assemble every branch of the `luma` namespace for one scope.
///
/// `graph_run` is the caller's latest evaluation, if any — the provider decides
/// whether it is still compatible with `scope` (design §11.3) rather than
/// trusting the caller. `store` must be rooted at the thread's workspace: every
/// artifact this writes lands under `<workspace>/inputs/`.
///
/// `luma.meta` and `luma.window` are **not** emitted here — the Python worker
/// synthesizes them from the manifest envelope (contract C1 / appendix A.4).
pub async fn assemble_bindings(
    pool: &SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    scope: &BindingScope,
    graph_run: Option<&GraphRunContribution>,
    store: &mut ArtifactStore,
) -> Result<BindingManifest, String> {
    let mut builder = BindingBuilder::new(scope.agent_kind()?, scope.analysis_scope());

    // A track id that doesn't resolve is a scope error the agent should see as
    // an unavailable branch, not as a hard failure.
    let track = match scope.track_id.as_deref() {
        Some(id) => crate::database::local::tracks::get_track_by_id(pool, id).await?,
        None => None,
    };
    let ctx = ProviderCtx {
        pool,
        storage,
        resource_root,
        scope,
        track,
    };

    track::provide(&mut builder, &ctx).await?;
    audio::provide(&mut builder, &ctx, store).await?;
    features::provide(&mut builder, &ctx, store).await?;
    venue::provide(&mut builder, &ctx, store).await?;
    score::provide(&mut builder, &ctx).await?;
    patterns::provide(&mut builder, &ctx).await?;
    graph::provide(&mut builder, &ctx, store, graph_run)?;

    builder.build().map_err(String::from)
}

// ---------------------------------------------------------------------------
// Shared provider helpers
// ---------------------------------------------------------------------------

/// Mark a path unavailable. Sugar over the builder so providers read as a list
/// of decisions rather than a list of `?`s.
pub(crate) fn unavailable(
    b: &mut BindingBuilder,
    path: &str,
    reason: impl Into<String>,
) -> Result<(), String> {
    b.unavailable(path, reason).map_err(String::from)?;
    Ok(())
}

pub(crate) fn inline<T: serde::Serialize>(
    b: &mut BindingBuilder,
    path: &str,
    value: T,
) -> Result<(), String> {
    b.inline(path, value).map_err(String::from)?;
    Ok(())
}

/// Shape implied by a tensor's axes. Passing both is an invitation to have them
/// disagree; every axis kind already knows its own length.
fn shape_of(axes: &[AxisSpec]) -> Vec<usize> {
    axes.iter().map(|a| a.len().unwrap_or(0)).collect()
}

/// Write `data` as a headerless little-endian f32 artifact and bind it at `path`.
pub(crate) fn put_f32(
    b: &mut BindingBuilder,
    store: &mut ArtifactStore,
    path: &str,
    data: &[f32],
    axes: Vec<AxisSpec>,
    unit: Option<&str>,
    provenance: Provenance,
) -> Result<(), String> {
    let descriptor = store.write_raw_f32(data).map_err(String::from)?;
    let mut tensor = TensorRef::new(
        descriptor.id.clone(),
        DType::F32,
        shape_of(&axes),
        axes,
        provenance,
    );
    if let Some(u) = unit {
        tensor = tensor.with_unit(u);
    }
    b.artifact(descriptor).map_err(String::from)?;
    b.tensor(path, tensor).map_err(String::from)?;
    Ok(())
}

/// As [`put_f32`] for f64 data (bar times, chord sections — stored as f64 upstream).
pub(crate) fn put_f64(
    b: &mut BindingBuilder,
    store: &mut ArtifactStore,
    path: &str,
    data: &[f64],
    axes: Vec<AxisSpec>,
    unit: Option<&str>,
    provenance: Provenance,
) -> Result<(), String> {
    let descriptor = store.write_raw_f64(data).map_err(String::from)?;
    let mut tensor = TensorRef::new(
        descriptor.id.clone(),
        DType::F64,
        shape_of(&axes),
        axes,
        provenance,
    );
    if let Some(u) = unit {
        tensor = tensor.with_unit(u);
    }
    b.artifact(descriptor).map_err(String::from)?;
    b.tensor(path, tensor).map_err(String::from)?;
    Ok(())
}

/// As [`put_f32`] for integer data (bar indices).
pub(crate) fn put_i64(
    b: &mut BindingBuilder,
    store: &mut ArtifactStore,
    path: &str,
    data: &[i64],
    axes: Vec<AxisSpec>,
    provenance: Provenance,
) -> Result<(), String> {
    let descriptor = store.write_raw_i64(data).map_err(String::from)?;
    let tensor = TensorRef::new(
        descriptor.id.clone(),
        DType::I64,
        shape_of(&axes),
        axes,
        provenance,
    );
    b.artifact(descriptor).map_err(String::from)?;
    b.tensor(path, tensor).map_err(String::from)?;
    Ok(())
}

/// An `[event]` tensor of absolute track seconds — the shape beats, downbeats
/// and drum onsets all share (design §8.3).
pub(crate) fn put_event_times(
    b: &mut BindingBuilder,
    store: &mut ArtifactStore,
    path: &str,
    times: &[f32],
    provenance: Provenance,
) -> Result<(), String> {
    put_f32(
        b,
        store,
        path,
        times,
        vec![AxisSpec::index("event", times.len())],
        Some("s"),
        provenance,
    )
}

/// The honest reason a preprocessed artifact is missing: the worker's own error
/// when it failed, otherwise "has not run" (report §7e).
pub(crate) async fn missing_reason(
    pool: &SqlitePool,
    track_id: &str,
    preprocessor: &str,
    what: &str,
) -> String {
    match crate::preprocessing::failures::last_error(pool, track_id, preprocessor).await {
        Ok(Some(error)) => format!("{what} preprocessing failed: {error}"),
        _ => format!("{what} preprocessing has not run for this track"),
    }
}

#[cfg(test)]
pub(crate) mod tests;
