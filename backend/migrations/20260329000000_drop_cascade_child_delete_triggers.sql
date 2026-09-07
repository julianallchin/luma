-- Remove delete-sync triggers for tables that are only ever deleted via
-- CASCADE from their parent (tracks). Supabase also has ON DELETE CASCADE,
-- so soft-deleting the parent is sufficient. Pushing soft-deletes for these
-- children causes RLS errors (uid missing from upsert payload) and
-- conflict-key mismatches on composite PKs.

DROP TRIGGER IF EXISTS sync_delete_track_beats;
DROP TRIGGER IF EXISTS sync_delete_track_roots;
DROP TRIGGER IF EXISTS sync_delete_track_stems;
DROP TRIGGER IF EXISTS sync_delete_track_scores;
