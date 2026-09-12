//! Readable sync state.
//!
//! PowerSync owns the queue, the retries and the backoff, so there is nothing
//! here for the app to decide — this is a report, not a control surface. A
//! write that the server refuses outright is recorded in the local-only
//! `sync_rejections` table rather than surfaced as a per-row failure list: it
//! needs recovery, not a retry button.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// A sync stream is open to the PowerSync service.
    pub connected: bool,
    /// Local writes are being sent to Supabase.
    pub uploading: bool,
    /// Remote rows are being applied locally.
    pub downloading: bool,
    /// When both directions last went quiet while connected, RFC 3339.
    pub last_synced_at: Option<String>,
    /// Rows waiting in the upload queue.
    pub pending_uploads: usize,
    /// The last transport error, if the connection is unhealthy now.
    pub error: Option<String>,
}

/// Counts are scoped to the named phase. `None` means its total is not yet known.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub phase: String,
    pub completed: usize,
    pub total: Option<usize>,
    pub unit: String,
}
