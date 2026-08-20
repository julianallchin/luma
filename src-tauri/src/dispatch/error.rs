//! The one error vocabulary at the command boundary.

use thiserror::Error;

/// Why a command did not produce a result.
///
/// A closed vocabulary over a wire contract that is prose: each variant carries
/// the exact message the frontend receives, and the Tauri adapter lowers it
/// with `From<CommandError> for String`.
///
/// Two properties to rely on:
///
/// - `Display` is a **verbatim passthrough**. Never add a prefix, and never
///   nest one `CommandError` inside another — the string is the contract and a
///   wrapper would corrupt it. Structure belongs in the variant, not the prose.
/// - `From<String>` lets a body that calls `Result<_, String>` services compose
///   with `?` untouched, so retyping a handler is incremental.
///
/// This is the only error type the dispatch layer exposes. A per-domain wrapper
/// enum that re-nests this one would be a pass-through, not a layer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CommandError {
    /// The addressed resource — or the command name itself — does not exist.
    #[error("{0}")]
    NotFound(String),

    /// The caller holds no lease on the resource, or the admitted identity
    /// changed underneath the call.
    #[error("{0}")]
    Unauthorized(String),

    /// An optimistic-concurrency check lost. `expected` and `found` carry the
    /// two revisions structurally; `message` is the prose the wire carries.
    #[error("{message}")]
    Conflict {
        /// The revision the caller believed was current. `None` means the
        /// caller expected no prior state at all.
        expected: Option<String>,
        /// The revision that actually was current when the check ran.
        found: Option<String>,
        /// The prose the wire carries for this conflict.
        message: String,
    },

    /// The arguments could not be decoded, or they violate a precondition.
    #[error("{0}")]
    Invalid(String),

    /// Everything else, including any body whose failures are still untyped.
    #[error("{0}")]
    Internal(String),
}

/// Hand-written because `#[from]` cannot express it: `String` is not an
/// `Error`, so thiserror will not derive the conversion.
impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::Internal(message)
    }
}

impl CommandError {
    /// Stable machine-readable discriminant, for a host that serializes the
    /// structure rather than the prose.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "notFound",
            Self::Unauthorized(_) => "unauthorized",
            Self::Conflict { .. } => "conflict",
            Self::Invalid(_) => "invalid",
            Self::Internal(_) => "internal",
        }
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self::Internal(message.to_string())
    }
}

/// The one lowering to the wire. Keeping it single is what makes retyping a
/// handler invisible to the frontend.
impl From<CommandError> for String {
    fn from(error: CommandError) -> Self {
        error.to_string()
    }
}
