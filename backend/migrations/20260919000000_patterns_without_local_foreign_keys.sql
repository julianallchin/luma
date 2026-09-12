-- `patterns` loses the two foreign keys a download cannot satisfy.
--
-- A checkpoint is applied in one transaction with foreign keys on, so a
-- deferred reference to a row the sync rules never ship fails at the commit
-- and fails again on every retry: the device stops applying checkpoints.
--
-- `category_id` points at `pattern_categories`, which is local-only and so
-- never arrives. `score_id` points at the score a pattern was lifted from,
-- and a verified library pattern reaches every signed-in client while its
-- author's score reaches nobody but them. Postgres declares neither column a
-- foreign key; this is SQLite catching up.

PRAGMA legacy_alter_table = ON;

CREATE TABLE "patterns_new" (
    id TEXT PRIMARY KEY,
    uid TEXT,
    name TEXT NOT NULL,
    description TEXT,
    category_id TEXT,
    is_verified INTEGER NOT NULL DEFAULT 0,
    author_name TEXT,
    forked_from_id TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    category_name TEXT,
    score_id TEXT
);
INSERT INTO "patterns_new" ("id", "uid", "name", "description", "category_id", "is_verified", "author_name", "forked_from_id", "created_at", "updated_at", "category_name", "score_id") SELECT "id", "uid", "name", "description", "category_id", "is_verified", "author_name", "forked_from_id", "created_at", "updated_at", "category_name", "score_id" FROM "patterns";
DROP TABLE "patterns";
ALTER TABLE "patterns_new" RENAME TO "patterns";
CREATE INDEX patterns_score_id ON patterns(score_id);
CREATE TRIGGER patterns_updated_at AFTER UPDATE ON patterns FOR EACH ROW
BEGIN UPDATE patterns SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
