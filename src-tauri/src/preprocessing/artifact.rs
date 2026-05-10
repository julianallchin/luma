//! Typed artifact identifiers for the preprocessing DAG.
//!
//! Every preprocessor declares an output artifact and a list of input artifacts.
//! The scheduler builds a dependency graph by matching inputs to outputs.
//!
//! Wire names returned from [`Artifact::as_str`] surface in logs and
//! `preprocessing_failures` rows; they must remain stable. To replace a
//! preprocessor's algorithm, bump its `version()` instead of renaming.

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
    #[allow(dead_code)]
    Mert,
    /// Output of the n2n drum-onset preprocessor.
    #[allow(dead_code)]
    DrumOnsets,
    /// Reserved for the upcoming joint bar classifier.
    #[allow(dead_code)]
    BarClassifications,
}

impl Artifact {
    /// Stable wire name. Persisted in the database — never rename.
    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Artifact::Audio => "audio",
            Artifact::BeatGrid => "beat_grid",
            Artifact::Stems => "stems",
            Artifact::Roots => "roots",
            Artifact::Mert => "mert",
            Artifact::DrumOnsets => "drum_onsets",
            Artifact::BarClassifications => "bar_classifications",
        }
    }
}
