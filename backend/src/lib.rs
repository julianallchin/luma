pub mod agent;
pub mod agent_execution;
pub mod annotation_preview;
pub mod audio;
/// Opening a library, in the one order every host follows.
pub mod boot;
mod compositor;
pub mod controls;
/// Compile a saved score for read-only render replay and diagnostics.
pub use compositor::build_score_scene;
pub mod config;
pub mod database;
pub mod dispatch;
mod engine_dj;
pub mod error;
pub mod eval;
pub mod events;
pub mod fixtures;
mod inputs;
pub mod laser;
/// Where a test that fakes the analysis workers puts a stand-in genre model,
/// so the genre stage does not download the real one.
pub use preprocessing::workers::genre::process::MODEL_FILE_NAME as GENRE_MODEL_FILE_NAME;
pub mod headless_host;
pub mod host_audio;
pub mod models;
pub mod motors;
mod output;
mod perform;
mod preprocessing;
pub use preprocessing::AnalysisTaskGroup;
pub mod recording;
mod rekordbox;
mod render_engine;
pub mod resources;
mod rig_uv;
pub mod runtime;
pub mod services;
pub mod settings;
pub mod stage_render;
pub mod storage;
/// Record sync. `sync::service::Service` is the whole seam: a host hands it the
/// PowerSync database and the state pool, and it connects, reports status and
/// announces downloaded change. Public because the host that starts it lives
/// outside this crate.
pub mod sync;
#[cfg(test)]
mod test_support;
mod topo;
/// The venue graph: loading it, solving it, and converting what came before.
pub mod venue_graph;

/// The version shared by the GPUI host and backend tools.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// This build, as the server's history records it against every write: the
/// version, and the commit where the build was given one (a release or a
/// nightly, which share a version between tags).
#[must_use]
pub fn build() -> String {
    match option_env!("LUMA_COMMIT_SHA") {
        Some(commit) => format!("{VERSION}+{commit}"),
        None => VERSION.to_owned(),
    }
}
