//! Crate-internal error type for the agent data plane.
//!
//! The house style is `Result<T, String>` at command boundaries; this type is a
//! thin newtype over `String` so `?` works over `std::io::Error` internally and
//! converts into the house style at the edge.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPlaneError(pub String);

pub type Result<T> = std::result::Result<T, DataPlaneError>;

impl DataPlaneError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DataPlaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DataPlaneError {}

impl From<std::io::Error> for DataPlaneError {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}

impl From<serde_json::Error> for DataPlaneError {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}

impl From<DataPlaneError> for String {
    fn from(e: DataPlaneError) -> String {
        e.0
    }
}

impl From<String> for DataPlaneError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DataPlaneError {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Shorthand for an error result.
pub fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(DataPlaneError::new(msg))
}
