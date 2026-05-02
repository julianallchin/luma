//! Preprocessing DAG.
//!
//! Generic registry of audio-analysis preprocessors. Each preprocessor
//! declares its inputs / output / version; the scheduler topo-sorts them at
//! startup, queues stale runs for each track, and dispatches them with
//! bounded parallelism. See [`scheduler`] for orchestration semantics and
//! [`preprocessor`] for the trait contract.
//!
//! Completion is tracked **on the artifact rows themselves** via the
//! `processor_version` column on `track_beats` / `track_stems` /
//! `track_roots`. There is no separate state table: pulling an artifact
//! from sync automatically counts as completion. Failures are tracked
//! separately in the local-only `preprocessing_failures` table for
//! exponential-backoff retry behaviour.

pub mod artifact;
pub mod failures;
pub mod preprocessor;
pub mod registry;
pub mod scheduler;
pub mod workers;
