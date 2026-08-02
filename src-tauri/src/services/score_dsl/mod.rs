//! Lossless, deterministic codec for Luma track scores.
//!
//! The human grammar accepts presentation names and omitted identities. Git
//! worktrees use [`serialize_canonical`], which requires stable clip and
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

pub use convert::{
    build_registry, clips_to_canonical_document, clips_to_document, compile_draft_track_document,
    compile_import_track_document, decode_canonical_track_document, document_to_clips,
    parse_group_expression, pattern_names_from_document, track_document_to_canonical_dsl,
    track_document_to_exemplar_dsl, CompileError, CompileErrorCode, CompiledTrackImport,
    ImportTrackDocumentError, PatternNames, SourceCompileError, TrackDocumentSerializeError,
};
pub use error::{format_error, DslError, DslErrorCode, DslWarning};
pub(crate) use parser::parse_canonical;
pub use parser::{parse, ParseOptions, ParseResult};
pub use serializer::{
    format_number, serialize, serialize_canonical, serialize_exemplar, serialize_group_expression,
    SerializeError, SerializeOptions,
};
pub use tokenizer::{tokenize, Token, TokenKind};
pub use trivia_merge::{
    merge_document_trivia, TriviaField, TriviaMergeConflict, TriviaMergeConflictKind,
    TriviaMergeInput, TriviaMergeOutcome, TriviaMergePath, TriviaMergePathSegment,
    TriviaMergeValue,
};
pub use types::{
    Annotation, Arg, ArgValue, BarRange, Comment, Document, GroupExpr, Layer, Loc, PatternArgument,
    PatternDefinition, PatternRegistry, Span, TimeUnit, Trivia,
};
pub use workflow::{
    export_score_source_with_access, load_score_dsl_context, load_score_dsl_document_with_access,
    load_score_pattern_names, ScoreDslContext, ScoreDslExport, ScoreDslExportKind,
};

#[cfg(test)]
mod tests;
