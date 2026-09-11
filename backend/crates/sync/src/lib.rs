//! PowerSync SDK glue.
//!
//! One SQLite file, two connection pools: the application's SQLx pool and the
//! SDK's rusqlite pool. This crate is the only place that knows how to make
//! those two agree — update hooks so the SDK sees SQLx writes, blocking actor
//! tasks so the SDK's synchronous SQLite calls cannot stall the app runtime,
//! and upload completion inside the caller's transaction.
//!
//! Everything about *what* Luma syncs lives in `backend/src/sync/`.

pub mod powersync;
