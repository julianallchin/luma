//! What sync needs from its host, named once.
//!
//! Every field is a shared handle, so a clone is another view of the same host
//! rather than an independent one — which is what lets the background loop own
//! one.

use crate::dispatch::Events;
use crate::storage::StorageRoot;

/// The host services a media transfer reaches outside its own handles.
#[derive(Clone)]
pub struct SyncHost {
    /// Where downloaded audio, stems and album art land.
    pub storage: StorageRoot,
    /// Progress and `library-changed` notifications.
    pub events: Events,
}
