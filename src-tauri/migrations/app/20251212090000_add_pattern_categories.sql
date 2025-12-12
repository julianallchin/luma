-- Add pattern categories to app database

CREATE TABLE IF NOT EXISTS pattern_categories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TRIGGER IF NOT EXISTS pattern_categories_updated_at
AFTER UPDATE ON pattern_categories
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE pattern_categories SET updated_at = CURRENT_TIMESTAMP WHERE id = OLD.id;
END;

ALTER TABLE patterns ADD COLUMN category_id INTEGER REFERENCES pattern_categories(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS patterns_category_idx ON patterns(category_id);

