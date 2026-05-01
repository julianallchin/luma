//! Preprocessing DAG.
//!
//! Generic registry of audio-analysis preprocessors. Each preprocessor
//! declares its inputs / output / version; the scheduler topo-sorts them at
//! startup, queues stale runs for each track, and dispatches them with
//! bounded parallelism. See [`scheduler`] for orchestration semantics and
//! [`preprocessor`] for the trait contract.

pub mod artifact;
pub mod preprocessor;
pub mod registry;
pub mod scheduler;
pub mod state;
pub mod workers;
