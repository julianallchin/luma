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
    /// The stored session was revoked by Supabase; only a sign-in can
    /// replace it, so nothing here retries.
    SessionRevoked,
    /// The row cannot be delivered as it stands, and no retry can change that:
    /// an identity the remote column type cannot hold, or an immutable row that
    /// collided with different bytes. Distinct from `Parse` so the push
    /// boundary can tell "never" from "not yet" without reading the message.
    Unpushable(String),
    /// A remote tombstone was declined because the local row still owns
    /// authored state, durable history, local artifacts, or dependent rows.
    /// This is a decision, not a failure: retrying can only repeat it.
    RemoteDeleteRefused {
        table: String,
        id: String,
        reason: String,
    },
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
            SyncError::SessionRevoked => write!(f, "session revoked; sign in again"),
            SyncError::Unpushable(reason) => write!(f, "not deliverable: {reason}"),
            SyncError::RemoteDeleteRefused { table, id, reason } => write!(
                f,
                "remote tombstone for {table}.{id} requires an authored-state deletion: {reason}"
            ),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<sqlx::Error> for SyncError {
    fn from(e: sqlx::Error) -> Self {
        SyncError::Local(e.to_string())
    }
}

impl From<crate::database::local::auth::AuthError> for SyncError {
    fn from(error: crate::database::local::auth::AuthError) -> Self {
        use crate::database::local::auth::AuthError;
        match error {
            AuthError::SessionRevoked => SyncError::SessionRevoked,
            AuthError::Other(message) => SyncError::Local(message),
        }
    }
}

impl From<SyncError> for String {
    fn from(e: SyncError) -> String {
        e.to_string()
    }
}
