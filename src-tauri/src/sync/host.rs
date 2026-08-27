//! What sync needs from its host, named once.
//!
//! Sync used to take an `AppHandle` and reach through it for the storage root,
//! the event bus, and the in-memory stores a pull has to invalidate. Naming
//! them is what lets a non-Tauri host run a sync at all — and it keeps `AppHandle`, which can supply anything, from being an
//! open channel into the sync module.
//!
//! Every field is a shared handle, so a clone is another view of the same host,
//! not an independent one. That is what lets the background loop own one.

use std::sync::Arc;

use crate::agent::subagent::SubagentRegistry;
use crate::agent_execution::{GraphRunStore, PythonWorkspaceService};
use crate::dispatch::Events;
use crate::storage::StorageRoot;

/// The host services a sync operation reaches outside its own handles.
#[derive(Clone)]
pub struct SyncHost {
    /// Where downloaded audio, stems and album art land.
    pub storage: StorageRoot,
    /// Progress and `library-changed` notifications.
    pub events: Events,
    /// Python kernels whose bindings a pull can invalidate.
    pub workspaces: Arc<PythonWorkspaceService>,
    /// Published graph evaluations a pull can invalidate.
    pub graph_runs: Arc<GraphRunStore>,
    /// Live subagent leases, which tell a stranded authored workspace from one
    /// a running child is still writing.
    pub subagents: Arc<SubagentRegistry>,
}
