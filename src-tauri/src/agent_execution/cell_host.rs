//! One cell's capability table, and the supervision every host call shares.
//!
//! Python names a method; this decides which domain owns it and whether that
//! domain is in scope at all. Keeping the split here — rather than letting one
//! domain handler grow a second domain's methods — is what lets a graph thread
//! have `venue.render` without also having `track.apply`.

use std::future::Future;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::agent_execution::track_host::TrackHost;
use crate::agent_execution::venue_host::VenueHost;
use crate::agent_execution::worker_process::{HostCallContext, HostCallError, HostCallHandler};

/// Longest any single host call may run, independent of the cell's own
/// deadline. A call that outlives it is a bug in the host, not patience owed
/// to the agent.
pub(crate) const MAX_HOST_CALL_DURATION: Duration = Duration::from_secs(30);

/// How often a supervised call re-checks the cancellation flag.
const HOST_CANCEL_POLL: Duration = Duration::from_millis(25);

/// The domains one Python cell may call into.
///
/// Absent domains are absent *capabilities*: a thread with no venue in scope
/// gets a `forbidden`, never a silently empty picture.
pub struct CellHost {
    track: Option<TrackHost>,
    venue: Option<VenueHost>,
}

impl CellHost {
    /// `None` when the cell has no host capability at all, which is the
    /// difference between `run_cell_with_host` and `run_cell`.
    #[must_use]
    pub fn new(track: Option<TrackHost>, venue: Option<VenueHost>) -> Option<Self> {
        (track.is_some() || venue.is_some()).then_some(Self { track, venue })
    }
}

impl HostCallHandler for CellHost {
    fn handle(
        &self,
        method: &str,
        payload: Value,
        context: &HostCallContext,
    ) -> Result<Value, HostCallError> {
        match method.split_once('.').map(|(domain, _)| domain) {
            Some("track") => match &self.track {
                Some(track) => track.handle(method, payload, context),
                None => Err(HostCallError::new(
                    "forbidden",
                    "this thread has no authored track in scope",
                )),
            },
            Some("venue") => match &self.venue {
                Some(venue) => venue.handle(method, payload, context),
                None => Err(HostCallError::new(
                    "forbidden",
                    "this thread has no venue in scope",
                )),
            },
            _ => Err(HostCallError::new(
                "unknown_method",
                format!("unknown host method {method:?}"),
            )),
        }
    }
}

/// Run `operation` under the cell's cancellation flag and a hard time limit.
///
/// Cancellation is polled rather than awaited so that a host call which is
/// itself blocked in the database still stops within one poll of a Stop.
pub(crate) async fn supervise<T>(
    operation: impl Future<Output = Result<T, HostCallError>>,
    context: &HostCallContext,
    limit: Duration,
) -> Result<T, HostCallError> {
    tokio::pin!(operation);
    let timeout = tokio::time::sleep(limit);
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            biased;
            result = &mut operation => break result,
            _ = &mut timeout => {
                break Err(HostCallError::new(
                    "timeout",
                    format!("host call exceeded {:.0}s", limit.as_secs_f64()),
                ));
            }
            _ = tokio::time::sleep(HOST_CANCEL_POLL) => {
                if context.is_cancelled() {
                    break Err(HostCallError::new(
                        "cancelled",
                        "the host call was cancelled",
                    ));
                }
            }
        }
    }
}

/// The time budget one host call gets: whatever the cell has left, capped.
pub(crate) fn call_limit(context: &HostCallContext) -> Result<Duration, HostCallError> {
    Ok(context
        .remaining()
        .ok_or_else(|| HostCallError::new("timeout", "the cell deadline has expired"))?
        .min(MAX_HOST_CALL_DURATION))
}

/// A JSON payload as the domain's request type.
pub(crate) fn decode<T: for<'de> Deserialize<'de>>(payload: Value) -> Result<T, HostCallError> {
    serde_json::from_value(payload)
        .map_err(|error| HostCallError::new("invalid_request", error.to_string()))
}
