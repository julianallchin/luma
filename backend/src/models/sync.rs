//! Readable sync state.
//!
//! PowerSync owns the queue, the retries and the backoff, so there is nothing
//! here for the app to decide — this is a report, not a control surface. A
//! write that the server refuses outright is recorded in the local-only
//! `sync_rejections` table and reported here until it is dismissed: it needs
//! a person to notice, not a retry button.
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
    /// Every sync stream has delivered a whole checkpoint to this database
    /// since it was last cleared: what is here is what the account has. False
    /// from a sign-in until its first download lands, which is when an empty
    /// table means "not here yet" rather than "none".
    pub caught_up: bool,
    /// Rows waiting in the upload queue.
    pub pending_uploads: usize,
    /// Local changes the server refused that nobody has dismissed yet,
    /// newest first. The server's version of each row has replaced them.
    pub refused: Vec<SyncRefusal>,
    /// Audio, stems or art moving to or from storage, once the transfer has
    /// found files to move.
    pub media: Option<SyncProgress>,
}

/// One local change the server would not take.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRefusal {
    pub id: String,
    pub table: String,
    pub row_id: String,
    /// `PUT`, `PATCH` or `DELETE`.
    pub op: String,
    /// The HTTP status. A 2xx is a write the server's policies filtered to no
    /// row at all: PostgREST answers success and changes nothing.
    pub status: u16,
    /// When it was refused, RFC 3339.
    pub at: String,
}

impl SyncRefusal {
    /// The change in a few words, for the list a person debugging reads:
    /// "Edit to tracks" and the like. The server's own text is in the log.
    #[must_use]
    pub fn describe(&self) -> String {
        let change = match self.op.as_str() {
            "PUT" => "New row in",
            "PATCH" => "Edit to",
            "DELETE" => "Removal from",
            _ => "Change to",
        };
        format!("{change} {}", self.table)
    }
}

/// What the status bar says about sync. Four states, in this order of
/// precedence: a refused change outranks being offline, which outranks work
/// in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncSummary {
    UpToDate,
    Syncing,
    /// Not connected. Local changes wait in the queue and upload when the
    /// connection is back, so this is a fact, not a fault.
    Offline {
        pending: usize,
    },
    /// Changes the server refused that the user has not dismissed.
    NotSaved {
        count: usize,
    },
}

impl SyncStatus {
    /// Local work that is on this machine only: rows waiting in the upload
    /// queue (PowerSync keeps a row there until the server acknowledges it),
    /// and the files an upload phase has still to move. Downloads are not
    /// counted, because nothing is lost if they stop.
    #[must_use]
    pub fn unsynced(&self) -> usize {
        let files = self
            .media
            .as_ref()
            .filter(|media| media.uploading)
            .map_or(0, |media| {
                media
                    .total
                    .map_or(1, |total| total.saturating_sub(media.completed))
            });
        self.pending_uploads + files
    }

    #[must_use]
    pub fn summary(&self) -> SyncSummary {
        if !self.refused.is_empty() {
            SyncSummary::NotSaved {
                count: self.refused.len(),
            }
        } else if !self.connected {
            SyncSummary::Offline {
                pending: self.pending_uploads,
            }
        } else if self.uploading
            || self.downloading
            || self.pending_uploads > 0
            || self.media.is_some()
        {
            SyncSummary::Syncing
        } else {
            SyncSummary::UpToDate
        }
    }
}

impl std::fmt::Display for SyncSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UpToDate => f.write_str("Up to date"),
            Self::Syncing => f.write_str("Syncing"),
            Self::Offline { pending: 0 } => f.write_str("Offline"),
            Self::Offline { pending } => write!(f, "Offline · {pending} pending"),
            Self::NotSaved { count } => write!(f, "{count} not saved"),
        }
    }
}

/// Counts are scoped to the named phase. `None` means its total is not yet known.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub phase: String,
    pub completed: usize,
    pub total: Option<usize>,
    pub unit: String,
    /// The phase moves files from this machine up to storage: work that
    /// exists nowhere else until it finishes. A download can always be
    /// fetched again, so quitting waits on this and not on that.
    #[serde(default)]
    pub uploading: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal() -> SyncRefusal {
        SyncRefusal {
            id: "r".into(),
            table: "tracks".into(),
            row_id: "t".into(),
            op: "PATCH".into(),
            status: 204,
            at: "2026-11-08T00:00:00Z".into(),
        }
    }

    /// A refusal is the one state that needs a person, so it outranks the
    /// rest; being offline outranks work that cannot be in flight anyway.
    #[test]
    fn a_refusal_outranks_offline_which_outranks_syncing() {
        let mut status = SyncStatus {
            connected: false,
            uploading: true,
            pending_uploads: 3,
            refused: vec![refusal(), refusal()],
            ..SyncStatus::default()
        };
        assert_eq!(status.summary().to_string(), "2 not saved");
        status.refused.clear();
        assert_eq!(status.summary().to_string(), "Offline · 3 pending");
        status.connected = true;
        assert_eq!(status.summary().to_string(), "Syncing");
        status.uploading = false;
        status.pending_uploads = 0;
        assert_eq!(status.summary().to_string(), "Up to date");
    }

    /// Media moving through storage is sync too: it reads as syncing, and
    /// does not add a state of its own.
    #[test]
    fn a_media_transfer_reads_as_syncing() {
        let status = SyncStatus {
            connected: true,
            media: Some(SyncProgress {
                phase: "Uploading audio".into(),
                completed: 0,
                total: Some(2),
                unit: "files".into(),
                uploading: true,
            }),
            ..SyncStatus::default()
        };
        assert_eq!(status.summary(), SyncSummary::Syncing);
    }

    /// What only this machine has: queued rows and files still to go up. A
    /// download is not counted, however much of it is left.
    #[test]
    fn unsynced_counts_uploads_and_never_downloads() {
        let media = |uploading, completed| SyncProgress {
            phase: String::new(),
            completed,
            total: Some(5),
            unit: "files".into(),
            uploading,
        };
        let mut status = SyncStatus {
            pending_uploads: 2,
            media: Some(media(true, 1)),
            ..SyncStatus::default()
        };
        assert_eq!(status.unsynced(), 6);
        status.media = Some(media(false, 1));
        assert_eq!(status.unsynced(), 2);
        status.pending_uploads = 0;
        assert_eq!(status.unsynced(), 0);
    }
}
