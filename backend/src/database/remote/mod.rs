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
// Canonical authored graph and score payloads sync through the immutable
// revision tables. Live implementation and track-score rows remain projections
// and are intentionally not a second payload authority.

pub mod common;
pub mod queries;
