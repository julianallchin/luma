-- Simplify: tags are stored directly on groups as JSON array
-- Drop the complex fixture-based tag system
DROP TABLE IF EXISTS fixture_tag_assignments;
DROP TABLE IF EXISTS fixture_tags;

-- Add tags column to groups (JSON array of tag names)
ALTER TABLE fixture_groups ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
