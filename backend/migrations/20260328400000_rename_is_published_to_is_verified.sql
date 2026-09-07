-- Rename is_published to is_verified on patterns.
-- "Verified" patterns are admin-curated; discoverability for other patterns
-- comes from being used in a score (via the search_patterns RPC).
ALTER TABLE patterns RENAME COLUMN is_published TO is_verified;
