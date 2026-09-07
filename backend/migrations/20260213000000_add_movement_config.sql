-- Add movement_config column to fixture_groups (JSON-serialized MovementConfig)
ALTER TABLE fixture_groups ADD COLUMN movement_config TEXT;
