//! The backend's one push channel to its host.

use std::sync::Arc;

use serde::Serialize;

use crate::models::tracks::TrackImportProgress;

/// Everything the backend pushes to a host — the only push channel; there is
/// no subscription command. A host matches it exhaustively, so an event no
/// host handles, or a host listening for one nothing sends, does not compile.
///
/// The serde names are what a windowless host prints on stderr.
#[derive(Debug, Serialize)]
#[serde(tag = "event", content = "payload", rename_all = "kebab-case")]
pub enum Event {
    /// Phase-one and background-analysis progress of one import. Consumers
    /// branch on its typed fields, never on status prose.
    TrackImportState(TrackImportProgress),
    /// Downloaded writes landed in these synced tables.
    ReplicaChanged { tables: Vec<String> },
    /// The server refused the stored session for good. Nothing retried can
    /// change that; only a new sign-in replaces it.
    SessionRevoked,
}

/// Where a command's push notifications go. The desktop adapter forwards to
/// the native event sink; a headless host records or discards them.
///
/// **Emission cannot fail.** A command's write is committed before it emits, so
/// an undeliverable event cannot mean a failed command; encoding that in the
/// signature is cheaper than asking every call site to drop a `Result`.
///
/// `Send + Sync + 'static` is load-bearing: the import commands clone their
/// emitter into a spawned background task to report progress, so a sink has to
/// outlive the call that created it.
pub trait EventSink: Send + Sync + 'static {
    /// Deliver one event. Implementations must not block and must not panic.
    fn emit(&self, event: Event);
}

/// Cloneable handle to an [`EventSink`].
#[derive(Clone)]
pub struct Events(Arc<dyn EventSink>);

impl Events {
    /// Route events to `sink`.
    pub fn new<S: EventSink>(sink: S) -> Self {
        Self(Arc::new(sink))
    }

    /// Drop every event. The default for a host with no observer.
    #[must_use]
    pub fn discard() -> Self {
        Self::new(DiscardSink)
    }

    /// Emit one event. Infallible by design — see [`EventSink`].
    pub fn emit(&self, event: Event) {
        self.0.emit(event);
    }
}

impl std::fmt::Debug for Events {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Events")
    }
}

struct DiscardSink;

impl EventSink for DiscardSink {
    fn emit(&self, _event: Event) {}
}
