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
// Sync order (respecting foreign key dependencies):
// 1. venues
// 2. fixtures (depends on venues)
// 3. pattern_categories
// 4. patterns (depends on categories)
// 5. implementations (depends on patterns)
// 6. venue_implementation_overrides (depends on venues, patterns, implementations)
// 7. tracks
// 8. track_beats, track_roots, track_waveforms, track_stems (depend on tracks)
// 9. scores (depends on tracks, venues)
// 10. track_scores (depends on scores, patterns)

pub mod common;
pub mod queries;
