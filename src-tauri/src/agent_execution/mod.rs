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

pub mod artifacts;
pub mod bindings;
pub mod error;

pub use artifacts::{
    ArtifactDescriptor, ArtifactEncoding, ArtifactKind, ArtifactStore, ImportRequest,
};
pub use bindings::{
    AgentKind, AnalysisScope, AnalysisWindow, ArtifactId, AxisSpec, BindingBuilder,
    BindingManifest, BindingRevision, BindingValue, DType, Provenance, TensorRef,
};
pub use error::{DataPlaneError, Result};
