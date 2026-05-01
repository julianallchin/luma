//! Static list of registered preprocessors.
//!
//! Adding a new preprocessor is one trait impl in `workers/` plus one entry
//! here. The scheduler topo-sorts this list at startup; cycles panic at
//! that point with a clear message.

use std::sync::Arc;

use crate::preprocessing::preprocessor::PreprocessorRef;
use crate::preprocessing::workers;

/// Build the canonical preprocessor list. Order doesn't matter — the
/// scheduler topo-sorts by dependency graph.
pub fn registered_preprocessors() -> Vec<PreprocessorRef> {
    vec![
        Arc::new(workers::beat_grid::BeatGridPreprocessor),
        Arc::new(workers::stems::StemsPreprocessor),
        Arc::new(workers::roots::RootsPreprocessor),
    ]
}
