//! Thin Tauri boundary for the single Rust score DSL codec.

use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::database::local::scores as scores_db;
use crate::database::local::venue_access::{
    AuthorizedVenue, Read, VenueAccess, VenueResource, Write,
};
use crate::database::Db;
use crate::models::authored_state::AuthoredProjectedDocument;
use crate::services::authored_documents::AuthoredDocuments;
use crate::services::score_dsl::{
    compile_draft_track_document, export_score_source_with_access,
    load_score_dsl_document_with_access, CompileError, DslError, DslWarning, ScoreDslExportKind,
    SourceCompileError, Span,
};
use crate::services::track_edits::{
    check_track_candidate_for_connection, TrackEditError, TrackEditPlan, TrackScope,
};

const MAX_SOURCE_BYTES: usize = 6 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ScoreDslExportResponse {
    pub source: String,
    pub revision: String,
    pub clip_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "snake_case")]
pub enum ScoreDslDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ScoreDslDiagnostic {
    pub severity: ScoreDslDiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub formatted: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub end_line: Option<usize>,
    pub end_column: Option<usize>,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ScoreDslValidationResponse {
    pub valid: bool,
    pub base_revision: String,
    pub clip_count: Option<usize>,
    pub diagnostics: Vec<ScoreDslDiagnostic>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ScoreDslImportResponse {
    pub document_id: String,
    pub revision_id: String,
    pub changed: bool,
    pub document: AuthoredProjectedDocument,
}

#[tauri::command]
pub async fn score_dsl_export(
    db: State<'_, Db>,
    score_id: String,
    track_id: String,
    venue_id: String,
    include_clip_ids: bool,
) -> Result<ScoreDslExportResponse, String> {
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Score(&score_id)).await?;
    let score = scores_db::get_score(&mut access, &score_id).await?;
    require_score_scope(&score, &track_id, &venue_id)?;
    let owner_user_id = score.uid;
    let exported = export_score_source_with_access(
        &mut access,
        &TrackScope {
            score_id,
            track_id,
            venue_id,
        },
        owner_user_id.as_deref(),
        if include_clip_ids {
            ScoreDslExportKind::Canonical
        } else {
            ScoreDslExportKind::Exemplar
        },
    )
    .await?;
    Ok(ScoreDslExportResponse {
        source: exported.source,
        revision: exported.revision,
        clip_count: exported.clip_count,
    })
}

#[tauri::command]
pub async fn score_dsl_validate(
    db: State<'_, Db>,
    score_id: String,
    track_id: String,
    venue_id: String,
    source: String,
) -> Result<ScoreDslValidationResponse, String> {
    validate_source_size(&source)?;
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Score(&score_id)).await?;
    let score = scores_db::get_score(&mut access, &score_id).await?;
    require_score_scope(&score, &track_id, &venue_id)?;
    let owner_user_id = score.uid;
    let scope = TrackScope {
        score_id,
        track_id,
        venue_id,
    };
    let (current, context) =
        load_score_dsl_document_with_access(&mut access, &scope, owner_user_id.as_deref()).await?;
    let base_revision = current.revision.clone();

    let (candidate, warnings) = match compile_draft_track_document(
        &source,
        current.revision.clone(),
        &context.beat_grid,
        &context.registry,
    ) {
        Ok(value) => value,
        Err(SourceCompileError::Syntax(errors)) => {
            return Ok(ScoreDslValidationResponse {
                valid: false,
                base_revision,
                clip_count: None,
                diagnostics: errors
                    .iter()
                    .map(|error| diagnostic_from_syntax(error, &source))
                    .collect(),
            });
        }
        Err(SourceCompileError::Semantic(error)) => {
            return Ok(ScoreDslValidationResponse {
                valid: false,
                base_revision,
                clip_count: None,
                diagnostics: vec![diagnostic_from_compile(&error, &source)],
            });
        }
    };

    match check_track_candidate_for_connection(
        access.connection(),
        &scope,
        TrackEditPlan {
            base_revision: base_revision.clone(),
            candidate: candidate.clips.clone(),
        },
    )
    .await
    {
        Ok(_) => {
            let mut diagnostics: Vec<ScoreDslDiagnostic> = warnings
                .iter()
                .map(|warning| diagnostic_from_warning(warning, &source))
                .collect();
            diagnostics.sort_by_key(diagnostic_sort_key);
            Ok(ScoreDslValidationResponse {
                valid: true,
                base_revision,
                clip_count: Some(candidate.clips.len()),
                diagnostics,
            })
        }
        Err(TrackEditError::Invalid { message } | TrackEditError::Scope { message }) => {
            Ok(ScoreDslValidationResponse {
                valid: false,
                base_revision,
                clip_count: None,
                diagnostics: vec![diagnostic_without_span("invalid_score", message)],
            })
        }
        Err(error @ (TrackEditError::Conflict { .. } | TrackEditError::Storage { .. })) => {
            Err(error.to_string())
        }
    }
}

#[tauri::command]
pub async fn score_dsl_import(
    db: State<'_, Db>,
    authored_documents: State<'_, AuthoredDocuments>,
    score_id: String,
    track_id: String,
    venue_id: String,
    operation_id: String,
    source: String,
    base_revision: String,
) -> Result<ScoreDslImportResponse, String> {
    validate_source_size(&source)?;
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Score(&score_id)).await?;
    let score = scores_db::get_score(&mut access, &score_id).await?;
    require_score_scope(&score, &track_id, &venue_id)?;
    let owner_user_id = score.uid;
    drop(access);
    let applied = authored_documents
        .apply_score_source_for_scope(
            &db.0,
            owner_user_id.as_deref(),
            TrackScope {
                score_id,
                track_id,
                venue_id,
            },
            &operation_id,
            &source,
            &base_revision,
            "Import score source",
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(ScoreDslImportResponse {
        document_id: applied.document_id,
        revision_id: applied.revision_id,
        changed: applied.changed,
        document: applied.document,
    })
}

fn require_score_scope(
    score: &crate::models::scores::Score,
    track_id: &str,
    venue_id: &str,
) -> Result<(), String> {
    if score.track_id == track_id && score.venue_id == venue_id {
        Ok(())
    } else {
        Err("Venue resource not found".into())
    }
}

pub(crate) fn validate_source_size(source: &str) -> Result<(), String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "score DSL source is too large ({} bytes; maximum is {MAX_SOURCE_BYTES})",
            source.len()
        ));
    }
    Ok(())
}

fn diagnostic_from_syntax(error: &DslError, source: &str) -> ScoreDslDiagnostic {
    diagnostic_with_span(
        ScoreDslDiagnosticSeverity::Error,
        error.code.as_str(),
        &error.message,
        error.hint.as_deref(),
        error.span,
        source,
    )
}

fn diagnostic_from_compile(error: &CompileError, source: &str) -> ScoreDslDiagnostic {
    match error.span {
        Some(span) => diagnostic_with_span(
            ScoreDslDiagnosticSeverity::Error,
            error.code.as_str(),
            &error.message,
            None,
            span,
            source,
        ),
        None => diagnostic_without_span(error.code.as_str(), error.message.clone()),
    }
}

fn diagnostic_from_warning(warning: &DslWarning, source: &str) -> ScoreDslDiagnostic {
    diagnostic_with_span(
        ScoreDslDiagnosticSeverity::Warning,
        &warning.code,
        &warning.message,
        None,
        warning.span,
        source,
    )
}

fn diagnostic_without_span(code: &str, message: String) -> ScoreDslDiagnostic {
    ScoreDslDiagnostic {
        severity: ScoreDslDiagnosticSeverity::Error,
        code: code.to_owned(),
        formatted: format!("Error: {message}"),
        message,
        line: None,
        column: None,
        end_line: None,
        end_column: None,
        hint: None,
    }
}

fn diagnostic_with_span(
    severity: ScoreDslDiagnosticSeverity,
    code: &str,
    message: &str,
    hint: Option<&str>,
    span: Span,
    source: &str,
) -> ScoreDslDiagnostic {
    let label = match severity {
        ScoreDslDiagnosticSeverity::Error => "Error",
        ScoreDslDiagnosticSeverity::Warning => "Warning",
    };
    let line = span.start.line.max(1);
    let column = span.start.column + 1;
    let source_line = source.lines().nth(line - 1).unwrap_or("");
    let end_column = if span.end.line == line {
        span.end.column.max(span.start.column + 1)
    } else {
        source_line.chars().count().max(span.start.column + 1)
    };
    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());
    let underline = "^".repeat(end_column.saturating_sub(span.start.column).max(1));
    let mut formatted = vec![
        format!("{label} at line {line}, column {column}: {message}"),
        format!("{pad} |"),
        format!("{gutter} | {source_line}"),
        format!("{pad} | {}{underline}", " ".repeat(span.start.column)),
    ];
    if let Some(hint) = hint {
        formatted.push(format!("{pad} | {hint}"));
    }
    ScoreDslDiagnostic {
        severity,
        code: code.to_owned(),
        message: message.to_owned(),
        formatted: formatted.join("\n"),
        line: Some(line),
        column: Some(column),
        end_line: Some(span.end.line.max(line)),
        end_column: Some(end_column + 1),
        hint: hint.map(str::to_owned),
    }
}

fn diagnostic_sort_key(diagnostic: &ScoreDslDiagnostic) -> (usize, usize, u8) {
    (
        diagnostic.line.unwrap_or(usize::MAX),
        diagnostic.column.unwrap_or(usize::MAX),
        match diagnostic.severity {
            ScoreDslDiagnosticSeverity::Error => 0,
            ScoreDslDiagnosticSeverity::Warning => 1,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::score_dsl::{DslErrorCode, Loc};

    #[test]
    fn structured_diagnostic_is_one_indexed_and_model_readable() {
        let source = "solid(all) @1\nnext(all) @2";
        let diagnostic = diagnostic_from_syntax(
            &DslError {
                code: DslErrorCode::UnknownPattern,
                message: "unknown pattern \"solid\"".to_owned(),
                span: Span {
                    start: Loc {
                        line: 1,
                        column: 0,
                        offset: 0,
                    },
                    end: Loc {
                        line: 1,
                        column: 5,
                        offset: 5,
                    },
                },
                hint: Some("Available patterns: wash".to_owned()),
            },
            source,
        );
        assert_eq!(diagnostic.line, Some(1));
        assert_eq!(diagnostic.column, Some(1));
        assert_eq!(diagnostic.end_column, Some(6));
        assert!(diagnostic.formatted.contains("1 | solid(all) @1"));
        assert!(diagnostic.formatted.contains("^^^^^"));
        assert!(diagnostic.formatted.contains("Available patterns: wash"));
    }

    #[test]
    fn source_limit_is_byte_exact() {
        assert!(validate_source_size(&"a".repeat(MAX_SOURCE_BYTES)).is_ok());
        assert!(validate_source_size(&"a".repeat(MAX_SOURCE_BYTES + 1)).is_err());
    }
}
