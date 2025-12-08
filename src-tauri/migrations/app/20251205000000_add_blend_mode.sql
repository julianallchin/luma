-- Add blend_mode column to track_annotations
ALTER TABLE track_annotations ADD COLUMN blend_mode TEXT NOT NULL DEFAULT 'replace';
