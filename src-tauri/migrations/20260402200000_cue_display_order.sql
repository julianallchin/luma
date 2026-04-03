-- Add display_order column to cues table for UI pad ordering (independent of z_index / blend order)
ALTER TABLE cues ADD COLUMN display_order INTEGER NOT NULL DEFAULT 0;
