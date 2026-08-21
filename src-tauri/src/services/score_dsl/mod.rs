//! Lossless, deterministic codec for Luma track scores.
//!
//! The human grammar accepts presentation names and omitted identities. Durable
//! authored workspaces use [`serialize_canonical`], which requires stable clip and
//! pattern identities and emits explicit layer indices. The committed source
//! contains authored semantics only; a [`TrackDocument`](crate::services::track_edits::TrackDocument)
//! revision is deliberately not part of the file.

mod convert;
mod error;
mod parser;
mod serializer;
mod tokenizer;
mod trivia_merge;
mod types;
mod version;
mod workflow;

pub(crate) use convert::{
    clips_to_canonical_document, compile_draft_track_document, compile_import_track_document,
    decode_canonical_track_document, pattern_names_from_document, track_document_to_canonical_dsl,
    track_document_to_exemplar_dsl, CompileError, PatternNames, SourceCompileError,
};
pub(crate) use error::{DslError, DslWarning};
pub(crate) use parser::{parse_canonical, ParseResult};
pub(crate) use serializer::serialize_canonical;
pub(crate) use trivia_merge::{
    merge_document_trivia, merge_document_trivia_later_wins, TriviaField, TriviaMergeConflict,
    TriviaMergeConflictKind, TriviaMergeInput, TriviaMergePathSegment, TriviaMergeValue,
};
pub(crate) use types::{Comment, Document, Layer, PatternRegistry, Span, Trivia};
pub(crate) use workflow::{
    export_score_source_with_access, load_score_dsl_context, load_score_dsl_document_with_access,
    load_score_pattern_names, ScoreDslExportKind,
};
/// Reachable only from `handlers::score_dsl`'s tests, which sit outside this
/// module and so cannot name the private submodules.
#[cfg(test)]
pub(crate) use {error::DslErrorCode, types::Loc};

#[cfg(test)]
mod tests;
