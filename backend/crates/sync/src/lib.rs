//! PowerSync SDK glue: one SQLite file, two connection pools.
//!
//! The only place that knows how to make the application's SQLx pool and the
//! SDK's rusqlite pool agree — update hooks so the SDK sees SQLx writes,
//! blocking actor tasks so the SDK's synchronous calls cannot stall the app
//! runtime, and upload completion inside the caller's transaction.
//!
//! What Luma syncs lives in `backend/src/sync/`.

pub mod powersync;
