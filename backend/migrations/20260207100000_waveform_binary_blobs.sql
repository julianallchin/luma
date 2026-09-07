-- Migrate waveform storage from JSON text to binary blobs.
-- This reduces storage by ~5-6x and speeds up serialization/deserialization.
-- Existing waveform data is cleared and will be regenerated on next access.

-- Clear existing JSON waveform data (will be regenerated as binary)
DELETE FROM track_waveforms;

-- Drop the old JSON columns
ALTER TABLE track_waveforms DROP COLUMN preview_samples_json;
ALTER TABLE track_waveforms DROP COLUMN preview_colors_json;
ALTER TABLE track_waveforms DROP COLUMN preview_bands_json;
ALTER TABLE track_waveforms DROP COLUMN full_samples_json;
ALTER TABLE track_waveforms DROP COLUMN colors_json;
ALTER TABLE track_waveforms DROP COLUMN bands_json;

-- Add new BLOB columns
ALTER TABLE track_waveforms ADD COLUMN preview_samples_blob BLOB NOT NULL DEFAULT x'';
ALTER TABLE track_waveforms ADD COLUMN preview_colors_blob BLOB;
ALTER TABLE track_waveforms ADD COLUMN preview_bands_blob BLOB;
ALTER TABLE track_waveforms ADD COLUMN full_samples_blob BLOB;
ALTER TABLE track_waveforms ADD COLUMN colors_blob BLOB;
ALTER TABLE track_waveforms ADD COLUMN bands_blob BLOB;
