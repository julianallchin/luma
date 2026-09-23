-- Cues and the account pattern library leave the product. Clips play the
-- built-in forms and presets, and a score keeps its own local definitions in
-- `score_definitions`, which this migration does not touch.
--
-- Order: the clip-forms change set deletes these rows on the server first
-- (with `supabase/migrations/20260923100000_drop_cues_and_pattern_library.sql`
-- after it). Locally the drops below delete whatever rows remain.
--
-- A MIDI binding that fired a cue, or blacked cues out, has no action left to
-- run and would not decode, so it goes too.

DROP VIEW IF EXISTS auth_visible_patterns;

DELETE FROM midi_bindings
WHERE CASE WHEN json_valid(action_json) THEN json_extract(action_json, '$.type') END
      IN ('fireCue', 'blackout');

DROP TABLE IF EXISTS venue_implementation_overrides;
DROP TABLE IF EXISTS cues;
DROP TABLE IF EXISTS implementations;
DROP TABLE IF EXISTS patterns;
DROP TABLE IF EXISTS pattern_categories;

-- Leftovers of the row-model cutover; nothing reads them.
DROP TABLE IF EXISTS scores_pending_cutover;
DROP TABLE IF EXISTS legacy_scores_backup;
