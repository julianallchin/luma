-- Add storage path for album art sync via Supabase Storage.
-- Follows the same pattern as storage_path for audio files.
ALTER TABLE tracks ADD COLUMN album_art_storage_path TEXT;
