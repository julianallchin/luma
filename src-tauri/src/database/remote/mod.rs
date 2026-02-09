// Remote database operations for Supabase cloud sync
//
// Each module handles CRUD operations for syncing local data to Supabase.
// All operations use the Supabase REST API (PostgREST).
//
// Key concepts:
// - local `remote_id` stores the cloud's BIGINT id as a string
// - Upsert operations return the cloud ID for updating local remote_id
// - Foreign keys must be resolved before syncing (e.g., venue's remote_id before fixtures)
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
// 9. scores (depends on tracks)
// 10. track_scores (depends on scores, patterns)

pub mod common;

pub mod categories;
pub mod fixtures;
pub mod implementations;
pub mod overrides;
pub mod patterns;
pub mod scores;
pub mod track_beats;
pub mod track_roots;
pub mod track_scores;
pub mod track_stems;
pub mod track_waveforms;
pub mod tracks;
pub mod venues;
