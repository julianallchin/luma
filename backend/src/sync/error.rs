use std::fmt;

/// What media transfer can fail with. Record replication has its own error
/// type in the SDK; this is only the bytes.
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
    /// Authentication required or token expired
    AuthRequired,
    /// The stored session was revoked by Supabase; only a sign-in can
    /// replace it, so nothing here retries.
    SessionRevoked,
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncError::Network(msg) => write!(f, "network error: {msg}"),
            SyncError::Api { status, message } => write!(f, "API error {status}: {message}"),
            SyncError::Parse(msg) => write!(f, "parse error: {msg}"),
            SyncError::Local(msg) => write!(f, "local DB error: {msg}"),
            SyncError::AuthRequired => write!(f, "authentication required"),
            SyncError::SessionRevoked => write!(f, "session revoked; sign in again"),
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

impl From<crate::database::remote::common::SyncError> for SyncError {
    fn from(error: crate::database::remote::common::SyncError) -> Self {
        use crate::database::remote::common::SyncError as Remote;
        match error {
            Remote::RequestFailed(message) => SyncError::Network(message),
            Remote::ApiError { status, message } => SyncError::Api { status, message },
            Remote::ParseError(message) => SyncError::Parse(message),
        }
    }
}

impl From<SyncError> for String {
    fn from(e: SyncError) -> String {
        e.to_string()
    }
}
