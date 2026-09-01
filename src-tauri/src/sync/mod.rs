//! Sync engine: bidirectional SQLite ↔ Supabase relational synchronization.
//!
//! Local SQLite is the source of truth in both directions. Writes go to SQLite
//! and stay there; push re-derives what the server is owed from the rows
//! themselves every cycle, and a deletion — the one fact a table cannot hold,
//! because the row is gone — is recorded in `sync_tombstones`. On startup (and
//! periodically), a pull fetches remote changes into local SQLite using
//! commit-ordered server sequence cursors.
//!
//! Authored documents use the same transport: immutable canonical revision rows
//! are pushed when their delivery marker is unset, while the one contended head
//! pointer is advanced only by the three server RPCs in `authored_remote`.
//! Conversation messages and append receipts are immutable traces as well.
//!
//! The engine is schema-agnostic — table metadata lives in `registry.rs` and
//! the pull path builds SQL dynamically from column lists. Adding a new
//! relational table requires both a `TableMeta` entry and a `Syncable` payload.

pub mod authored_remote;
pub mod error;
pub mod pull;
pub mod push;
pub mod push_state;
pub mod registry;
pub mod state;
pub mod traits;

pub mod files;
pub mod host;
pub mod orchestrator;
pub mod supabase_remote;
pub mod tombstone;
pub mod transition;

#[cfg(test)]
mod tests;
