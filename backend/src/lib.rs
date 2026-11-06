pub mod agent;
pub mod agent_execution;
pub mod annotation_preview;
pub mod audio;
mod canonical_json;
mod compositor;
pub mod controls;
/// Compile a saved score for read-only render replay and diagnostics.
pub use compositor::build_score_scene;
pub mod config;
pub mod database;
pub mod dispatch;
mod engine_dj;
pub mod eval;
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
mod rig_uv;
mod rekordbox;
mod render_engine;
pub mod resources;
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
mod topo;
/// The venue graph: loading it, solving it, and converting what came before.
pub mod venue_graph;

/// The version shared by the GPUI host and backend tools.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
