//! Sync engine: bidirectional SQLite ↔ Supabase relational synchronization.
//!
//! Local SQLite is the source of truth for reads. Writes go to SQLite first,
//! then are enqueued in `pending_ops` and flushed to Supabase by a background
//! worker. On startup (and periodically), a pull fetches remote changes into
//! local SQLite using delta timestamps.
//!
//! Authored score clips and implementation graphs are intentionally outside
//! this engine. Git `main` is their sole current-state authority; SQLite holds
//! only its live projection. Authenticated Git object/ref transport must remain
//! a separate protocol so generic row sync can never shadow-write those blobs.
//!
//! The engine is schema-agnostic — table metadata lives in `registry.rs` and
//! the pull path builds SQL dynamically from column lists. Adding a new
//! relational table requires both a `TableMeta` entry and a `Syncable` payload.

pub mod error;
pub mod pending;
pub mod pull;
pub mod push;
pub mod registry;
pub mod state;
pub mod traits;

pub mod files;
pub mod orchestrator;
pub mod supabase_remote;

#[cfg(test)]
mod tests;
