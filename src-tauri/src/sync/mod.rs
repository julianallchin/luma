//! Sync engine: bidirectional SQLite ↔ Supabase relational synchronization.
//!
//! Local SQLite is the source of truth for reads. Writes go to SQLite first,
//! then are enqueued in `pending_ops` and flushed to Supabase by a background
//! worker. On startup (and periodically), a pull fetches remote changes into
//! local SQLite using commit-ordered server sequence cursors.
//!
//! Authored documents use the same transport: immutable canonical revision
//! rows travel through `pending_ops`, while the one contended head pointer is
//! advanced only by the three server RPCs in `authored_remote`. Conversation
//! messages and append receipts are immutable traces as well.
//!
//! The engine is schema-agnostic — table metadata lives in `registry.rs` and
//! the pull path builds SQL dynamically from column lists. Adding a new
//! relational table requires both a `TableMeta` entry and a `Syncable` payload.

pub mod authored_remote;
pub mod error;
pub mod pending;
pub mod pull;
pub mod push;
pub mod registry;
pub mod state;
pub mod traits;

pub mod files;
pub mod host;
pub mod orchestrator;
pub mod supabase_remote;

#[cfg(test)]
mod tests;
