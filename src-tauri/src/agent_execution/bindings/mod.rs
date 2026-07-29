//! Binding manifests: the typed description of everything the agent's Python
//! namespace can see, plus the builder domain providers write into.

pub mod assembler;
pub mod manifest;

pub use assembler::BindingBuilder;
pub use manifest::{
    AgentKind, AnalysisScope, AnalysisWindow, ArtifactId, AxisSpec, BindingManifest,
    BindingRevision, BindingValue, Coordinates, DType, Provenance, TensorRef, SCHEMA_VERSION,
};
