//! What sync needs from its host, named once.
//!
//! Sync used to take an `AppHandle` and reach through it for the storage root,
//! the event bus, and the in-memory stores a pull has to invalidate. Naming
//! them is what lets a non-Tauri host run a sync at all — and it keeps `AppHandle`, which can supply anything, from being an
//! open channel into the sync module.
//!
//! Every field is a shared handle, so a clone is another view of the same host,
//! not an independent one. That is what lets the background loop own one.

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
