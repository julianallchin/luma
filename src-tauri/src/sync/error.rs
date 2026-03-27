use std::fmt;

/// Unified error type for the sync engine.
#[derive(Debug)]
pub enum SyncError {
    /// HTTP request to Supabase failed
    Network(String),
    /// Supabase API returned a non-success status
    Api { status: u16, message: String },
    /// Failed to parse a response from Supabase
    Parse(String),
    /// Local SQLite operation failed
    Local(String),
    /// Required field was missing (e.g., uid on a record)
    MissingField(String),
    /// Record not found locally or remotely
    NotFound { table: String, id: String },
    /// Authentication required or token expired
    AuthRequired,
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncError::Network(msg) => write!(f, "network error: {msg}"),
            SyncError::Api { status, message } => write!(f, "API error {status}: {message}"),
            SyncError::Parse(msg) => write!(f, "parse error: {msg}"),
            SyncError::Local(msg) => write!(f, "local DB error: {msg}"),
            SyncError::MissingField(field) => write!(f, "missing field: {field}"),
            SyncError::NotFound { table, id } => write!(f, "{table} {id} not found"),
            SyncError::AuthRequired => write!(f, "authentication required"),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<sqlx::Error> for SyncError {
    fn from(e: sqlx::Error) -> Self {
        SyncError::Local(e.to_string())
    }
}

impl From<SyncError> for String {
    fn from(e: SyncError) -> String {
        e.to_string()
    }
}
