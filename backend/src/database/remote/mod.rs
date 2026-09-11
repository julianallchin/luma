//! Direct PostgREST access to Supabase, for the few operations that are a
//! request/response rather than replication: joining a venue by share code,
//! publishing a share code, and leaving a venue.
//!
//! Row replication does not come through here — that is
//! [`crate::sync`]'s job.

pub mod common;
