//! Typed artifact identifiers for the preprocessing DAG.
//!
//! Every preprocessor declares an output artifact and a list of input artifacts.
//! The scheduler builds a dependency graph by matching inputs to outputs.
//!
//! Wire names returned from [`Artifact::as_str`] are persisted in the
//! `preprocessing_runs` table; they must remain stable. To replace a
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
    /// Reserved for the upcoming ADTOF drum-onset preprocessor.
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
            Artifact::DrumOnsets => "drum_onsets",
            Artifact::BarClassifications => "bar_classifications",
        }
    }
}
