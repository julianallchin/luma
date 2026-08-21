//! The agent code-execution data plane.
//!
//! One manifest describes everything the agent's Python namespace can see; one
//! artifact store owns the bytes behind it. This module is deliberately pure
//! Rust — no Tauri, no SQLite, no app services — so that domain providers, the
//! headless harness and the tests can all use it the same way.
//!
//! - [`bindings::manifest`] — the wire types (contract C1) and their exact JSON;
//! - [`bindings::assembler`] — the builder domain providers write into, plus the
//!   cross-provider invariants;
//! - [`artifacts::store`] — the per-thread workspace, imports, leases, cleanup;
//! - [`artifacts::codecs`] — `raw_le`, `npy`, `pcm_f32` and `png`.

//! - [`worker_launcher`] / [`sandbox`] — how a worker process gets started;
//! - [`worker_process`] — the NDJSON protocol client and the interrupt ladder;
//! - [`workspace`] — one workspace and one kernel per agent thread.

pub mod artifacts;
pub mod bindings;
pub mod error;
pub mod graph_runs;
#[cfg(test)]
mod kernel_tests;
pub mod sandbox;
#[cfg(all(test, target_os = "macos"))]
mod sandbox_tests;
pub mod tauri_env;
pub mod thread_cleanup;
pub mod track_host;
pub mod worker_launcher;
pub mod worker_process;
pub mod workspace;

pub use artifacts::{
    ArtifactDescriptor, ArtifactEncoding, ArtifactKind, ArtifactStore, ImportRequest,
};
pub use bindings::{
    AgentKind, AnalysisScope, AnalysisWindow, ArtifactId, AxisSpec, BindingBuilder,
    BindingManifest, BindingRevision, BindingValue, DType, Provenance, TensorRef,
};
pub use error::{DataPlaneError, Result};
pub use graph_runs::GraphRunStore;
pub use worker_launcher::{SandboxPolicy, WorkerLauncher};
pub use worker_process::{
    CancelToken, ExecOutcome, ExecStatus, HostCallError, HostCallHandler, WorkerConfig,
    WorkerHandle,
};
pub use workspace::{CellOutcome, PythonWorkspaceService, WorkerEnv, Workspace};
