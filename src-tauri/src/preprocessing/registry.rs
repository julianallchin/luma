//! Static list of registered preprocessors.
//!
//! Adding a new preprocessor is one trait impl in `workers/` plus one entry
//! here. The scheduler topo-sorts this list at startup; cycles panic at
//! that point with a clear message.

use std::collections::HashMap;
use std::sync::Arc;

use crate::preprocessing::preprocessor::PreprocessorRef;
use crate::preprocessing::workers;

/// Build the canonical preprocessor list. Order doesn't matter — the
/// scheduler topo-sorts by dependency graph.
pub fn registered_preprocessors() -> Vec<PreprocessorRef> {
    vec![
        Arc::new(workers::beat_grid::BeatGridPreprocessor),
        Arc::new(workers::stems::StemsPreprocessor),
        Arc::new(workers::mert::MertPreprocessor),
        Arc::new(workers::roots::RootsPreprocessor),
        Arc::new(workers::n2n::N2NPreprocessor),
        Arc::new(workers::classifier::ClassifierPreprocessor),
    ]
}

/// Current `version()` keyed by `artifact_table()` for every registered
/// preprocessor. Callers (e.g., the track browser query) use this to gate
/// "is this artifact up-to-date?" checks against the version a row was
/// produced at, so a version bump correctly invalidates stale rows in the UI.
pub fn current_artifact_versions() -> HashMap<&'static str, u32> {
    registered_preprocessors()
        .iter()
        .map(|p| (p.artifact_table(), p.version()))
        .collect()
}
