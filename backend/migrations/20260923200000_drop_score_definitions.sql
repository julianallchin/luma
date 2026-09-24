-- Every clip plays a shipped form, so a score has no local definitions.
--
-- Order: the clip-forms change set deletes the `score_definitions` rows on the
-- server first (with `supabase/migrations/20260923200000_drop_score_definitions.sql`
-- after it). Locally the drop below deletes whatever rows remain.
--
-- A draft stores a whole score document. Documents written before this change
-- carry `version` and `definitions`, which no longer decode, so every draft
-- goes. A draft is a subagent's unmerged scratch copy.

DROP TRIGGER IF EXISTS score_definitions_updated_at;
DROP TABLE IF EXISTS score_definitions;

DELETE FROM drafts;
