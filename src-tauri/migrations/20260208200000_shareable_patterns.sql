ALTER TABLE patterns ADD COLUMN is_published INTEGER NOT NULL DEFAULT 0;
ALTER TABLE patterns ADD COLUMN author_name TEXT;
ALTER TABLE patterns ADD COLUMN forked_from_remote_id TEXT;
