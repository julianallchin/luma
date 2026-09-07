-- Replace category_id FK with category_name text on patterns.
-- Categories are a hardcoded set — storing the name directly eliminates
-- UUID mismatch issues between local and remote databases.

-- Backfill category_name from existing FK before dropping the column
ALTER TABLE patterns ADD COLUMN category_name TEXT;

UPDATE patterns SET category_name = (
    SELECT pc.name FROM pattern_categories pc WHERE pc.id = patterns.category_id
) WHERE category_id IS NOT NULL;

-- SQLite doesn't support DROP COLUMN on older versions, but the column
-- will simply be ignored by queries that don't select it. The sync registry
-- and all queries now use category_name instead of category_id.
