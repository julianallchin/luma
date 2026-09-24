-- Columns nothing reads any more.
--
-- - `agent_threads.implementation_id` named the library implementation a
--   pattern-graph thread authored. The library is gone; the column is always
--   NULL.
-- - `midi_bindings.mode_json` and `target_override_json`, and
--   `midi_modifiers.groups_json`, were for cue triggering. No MIDI action reads
--   them since cues were removed.
--
-- No persistent trigger or view names these columns, and the upload-queue
-- triggers are TEMP triggers of the app's writer connections, so a plain
-- DROP COLUMN works. The index goes first because it names its column.
DROP INDEX IF EXISTS idx_agent_threads_graph_implementation;
ALTER TABLE agent_threads DROP COLUMN implementation_id;
ALTER TABLE midi_bindings DROP COLUMN mode_json;
ALTER TABLE midi_bindings DROP COLUMN target_override_json;
ALTER TABLE midi_modifiers DROP COLUMN groups_json;
