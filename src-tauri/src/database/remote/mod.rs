// Remote database operations for Supabase cloud sync
//
// Each module handles CRUD operations for syncing local data to Supabase.
// All operations use the Supabase REST API (PostgREST).
//
// Key concepts:
// - Local and cloud share the same UUID primary key (no remote_id mapping)
// - Upsert operations use ON CONFLICT on id (UUID PK)
// - Foreign keys use the same UUIDs in both local and cloud
//
// The authoritative sync order is derived from `sync::registry::TABLES`.
// Authored graph and score documents are intentionally absent from generic
// relational sync: implementations, track scores, and implementation-routing
// pointers require the separate authenticated Git authored-state transport
// rather than a second row-shaped authority.

pub mod common;
pub mod queries;
