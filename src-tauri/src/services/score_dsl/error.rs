use std::fmt;

use super::types::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DslErrorCode {
    UnknownPattern,
    UnknownArg,
    TypeMismatch,
    MissingSelection,
    InvalidBarRange,
    InvalidBlendMode,
    UnexpectedToken,
    UnexpectedEof,
    InvalidHexColor,
    DuplicateBarRange,
    EmptyBarBlock,
    InvalidCanonicalSchema,
}

impl DslErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownPattern => "unknown_pattern",
            Self::UnknownArg => "unknown_arg",
            Self::TypeMismatch => "type_mismatch",
            Self::MissingSelection => "missing_selection",
            Self::InvalidBarRange => "invalid_bar_range",
            Self::InvalidBlendMode => "invalid_blend_mode",
            Self::UnexpectedToken => "unexpected_token",
            Self::UnexpectedEof => "unexpected_eof",
            Self::InvalidHexColor => "invalid_hex_color",
            Self::DuplicateBarRange => "duplicate_bar_range",
            Self::EmptyBarBlock => "empty_bar_block",
            Self::InvalidCanonicalSchema => "invalid_canonical_schema",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DslError {
    pub code: DslErrorCode,
    pub message: String,
    pub span: Span,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DslWarning {
    pub code: String,
    pub message: String,
    pub span: Span,
}

pub fn format_error(error: &DslError, source: &str) -> String {
    let lines: Vec<&str> = source.split('\n').collect();
    let line = error.span.start.line;
    let column = error.span.start.column;
    let end_column = if error.span.end.line == line {
        error.span.end.column
    } else {
        lines
            .get(line.saturating_sub(1))
            .map_or(column + 1, |value| value.chars().count())
    };
    let underline_len = end_column.saturating_sub(column).max(1);
    let source_line = lines.get(line.saturating_sub(1)).copied().unwrap_or("");
    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());
    let mut output = vec![
        format!(
            "Error at line {line}, column {}: {}",
            column + 1,
            error.message
        ),
        format!("{pad} |"),
        format!("{gutter} | {source_line}"),
        format!(
            "{pad} | {}{}",
            " ".repeat(column),
            "^".repeat(underline_len)
        ),
    ];
    if let Some(hint) = &error.hint {
        output.push(format!("{pad} | {hint}"));
    }
    output.join("\n")
}

impl fmt::Display for DslError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for DslError {}
