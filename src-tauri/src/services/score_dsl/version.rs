use super::error::{DslError, DslErrorCode};
use super::types::{Loc, Span};

pub(super) const CURRENT_SCORE_SCHEMA_VERSION: u32 = 1;
const HEADER_MARKER: &str = "# luma-score-schema";
const HEADER_PREFIX: &str = "# luma-score-schema: ";

pub(super) fn canonical_header() -> String {
    format!("{HEADER_PREFIX}{CURRENT_SCORE_SCHEMA_VERSION}")
}

/// Validate the canonical file envelope and hide it from the score grammar.
/// Keeping a same-width blank first line preserves every source location in
/// the score body while ensuring the format marker never becomes user trivia.
pub(super) fn decode_current(source: &str) -> Result<String, DslError> {
    decode_envelope(source, true)
}

/// Accept either a human draft or a canonical file checked out from Git. The
/// marker namespace is reserved: once present it must be a valid current
/// envelope, otherwise a future/garbled file could be mistaken for a comment.
pub(super) fn decode_optional_current(source: &str) -> Result<String, DslError> {
    decode_envelope(source, false)
}

fn decode_envelope(source: &str, required: bool) -> Result<String, DslError> {
    let first_line = source.split_once('\n').map_or(source, |(line, _)| line);
    if !first_line.starts_with(HEADER_MARKER) {
        if required {
            return Err(schema_error(
                first_line,
                format!("canonical score must begin with {:?}", canonical_header()),
            ));
        }
        return Ok(source.to_owned());
    }
    let version_text = first_line
        .strip_prefix(HEADER_PREFIX)
        .ok_or_else(|| schema_error(first_line, "malformed canonical score envelope".to_owned()))?;
    let version = version_text.parse::<u32>().map_err(|_| {
        schema_error(
            first_line,
            "canonical score schema version must be a positive integer".to_owned(),
        )
    })?;
    if version_text != version.to_string() {
        return Err(schema_error(
            first_line,
            "canonical score schema version is not in canonical form".to_owned(),
        ));
    }
    if version != CURRENT_SCORE_SCHEMA_VERSION {
        return Err(schema_error(
            first_line,
            format!(
                "unsupported canonical score schema version {version}; this build supports version {CURRENT_SCORE_SCHEMA_VERSION}"
            ),
        ));
    }

    let mut decoded = " ".repeat(first_line.len());
    if let Some((_, body)) = source.split_once('\n') {
        decoded.push('\n');
        decoded.push_str(body);
    }
    Ok(decoded)
}

fn schema_error(first_line: &str, message: String) -> DslError {
    DslError {
        code: DslErrorCode::InvalidCanonicalSchema,
        message,
        span: Span {
            start: Loc {
                line: 1,
                column: 0,
                offset: 0,
            },
            end: Loc {
                line: 1,
                column: first_line.chars().count(),
                offset: first_line.len(),
            },
        },
        hint: Some(format!(
            "Expected the first line to be {:?}.",
            canonical_header()
        )),
    }
}
