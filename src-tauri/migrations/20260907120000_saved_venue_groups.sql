-- Generated groups become saved collections once, preserving their existing names.
ALTER TABLE venues ADD COLUMN groups_initialized INTEGER NOT NULL DEFAULT 0 CHECK (groups_initialized IN (0, 1));
