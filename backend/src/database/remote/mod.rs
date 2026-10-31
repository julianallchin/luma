//! Direct PostgREST access to Supabase, for the few operations that are a
//! request/response rather than replication: today, redeeming a venue's share
//! code.
//!
//! Row replication does not come through here — that is
//! [`crate::sync`]'s job.

pub mod common;
pub mod postgrest;
