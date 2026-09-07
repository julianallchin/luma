//! Typed artifact identifiers for the preprocessing DAG.
//!
//! Every preprocessor declares an output artifact and a list of input artifacts.
//! The scheduler builds a dependency graph by matching inputs to outputs.
//!
/// Logical artifact a preprocessor produces or consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Artifact {
    /// The raw imported audio file. Always available — never produced by a
    /// preprocessor — and may be listed as an input.
    Audio,
    /// Output of the beat-grid preprocessor (beats, downbeats, BPM).
    BeatGrid,
    /// Output of the stems preprocessor (Demucs separation files on disk).
    Stems,
    /// Output of the roots preprocessor (chord sections + logits).
    Roots,
    /// Cached MERT-95M layer-7 features for the full-mix track on disk
    /// (.npy at 75 Hz × 768-d, fp16). Shared by the bar classifier and the
    /// n2n drum-onset preprocessor — both consume it instead of running their
    /// own MERT extraction.
    Mert,
    /// Output of the n2n drum-onset preprocessor.
    DrumOnsets,
    /// Output of the joint bar classifier.
    BarClassifications,
    /// Output of the genre preprocessor: per-bar Discogs style activations
    /// plus a whole-track summary.
    Genre,
}
