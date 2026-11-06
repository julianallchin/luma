//! The registered preprocessors and the DAG they form.
//!
//! Adding a new preprocessor is one trait impl in `workers/` plus one entry
//! in [`registered_preprocessors`]. [`dag`] layers the list once and checks it
//! there: two producers of one artifact, an input nobody produces, or a cycle
//! panics on first use, which is startup reconciliation.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::PreprocessorRef;
use crate::preprocessing::workers;
use crate::topo;

/// Order doesn't matter — [`dag`] sorts by dependency.
fn registered_preprocessors() -> Vec<PreprocessorRef> {
    vec![
        Arc::new(workers::beat_grid::BeatGridPreprocessor),
        Arc::new(workers::stems::StemsPreprocessor),
        Arc::new(workers::mert::MertPreprocessor),
        Arc::new(workers::roots::RootsPreprocessor),
        Arc::new(workers::n2n::N2NPreprocessor),
        Arc::new(workers::classifier::ClassifierPreprocessor),
        Arc::new(workers::genre::GenrePreprocessor),
    ]
}

/// A layered topological ordering — `layers[i]` may run in parallel; layer
/// `i+1` depends only on outputs from layers `0..=i`.
pub struct Dag {
    layers: Vec<Vec<PreprocessorRef>>,
}

impl Dag {
    /// Layer `preprocessors` via the shared [`topo::layers`]. A
    /// preprocessor's parents are the preprocessors that produce its input
    /// artifacts; `Artifact::Audio` has no producer and is filtered out.
    fn new(preprocessors: Vec<PreprocessorRef>) -> Self {
        let mut producer_of: HashMap<Artifact, &'static str> = HashMap::new();
        for p in &preprocessors {
            if let Some(prev) = producer_of.insert(p.output(), p.name()) {
                panic!(
                    "Two preprocessors produce the same artifact {:?}: {} and {}",
                    p.output(),
                    prev,
                    p.name()
                );
            }
        }

        let parents_of = |p: &PreprocessorRef| -> Vec<&'static str> {
            p.inputs()
                .iter()
                .filter(|a| !matches!(a, Artifact::Audio))
                .map(|a| {
                    *producer_of.get(a).unwrap_or_else(|| {
                        panic!(
                            "Preprocessor {} depends on artifact {:?} which has no producer",
                            p.name(),
                            a
                        )
                    })
                })
                .collect()
        };

        let layers = topo::layers(&preprocessors, |p| p.name(), parents_of)
            .into_iter()
            .map(|layer| layer.into_iter().cloned().collect())
            .collect();
        Self { layers }
    }

    pub fn layers(&self) -> &[Vec<PreprocessorRef>] {
        &self.layers
    }

    /// Every registered preprocessor, in topological order.
    pub fn preprocessors(&self) -> impl Iterator<Item = &PreprocessorRef> {
        self.layers.iter().flatten()
    }
}

/// The registered preprocessors, layered and validated once per process.
pub fn dag() -> &'static Dag {
    static DAG: OnceLock<Dag> = OnceLock::new();
    DAG.get_or_init(|| Dag::new(registered_preprocessors()))
}

/// Current `version()` keyed by `artifact_table()` for every registered
/// preprocessor. Callers (e.g., the track browser query) use this to gate
/// "is this artifact up-to-date?" checks against the version a row was
/// produced at, so a version bump correctly invalidates stale rows in the UI.
pub fn current_artifact_versions() -> HashMap<&'static str, u32> {
    dag()
        .preprocessors()
        .map(|p| (p.artifact_table(), p.version()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every preprocessor runs in a strictly later layer than the producer of
    /// each artifact it reads, so its inputs exist by the time it starts.
    #[test]
    fn every_preprocessor_runs_after_the_producers_of_its_inputs() {
        let dag = dag();
        let layer_of = |artifact: Artifact| {
            dag.layers()
                .iter()
                .position(|layer| layer.iter().any(|p| p.output() == artifact))
        };
        for (layer, preprocessors) in dag.layers().iter().enumerate() {
            for p in preprocessors {
                for &input in p.inputs() {
                    if input == Artifact::Audio {
                        continue;
                    }
                    let producer = layer_of(input).expect("every input has a producer");
                    assert!(
                        producer < layer,
                        "{} (layer {layer}) reads {input:?}, produced in layer {producer}",
                        p.name()
                    );
                }
            }
        }
        assert_eq!(
            dag.preprocessors().count(),
            registered_preprocessors().len(),
            "every registered preprocessor is scheduled"
        );
    }
}
