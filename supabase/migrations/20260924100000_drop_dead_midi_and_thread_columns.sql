-- Columns nothing reads any more: `agent_threads.implementation_id` (the
-- pattern library's implementation, always NULL), and the cue-only MIDI
-- columns `midi_bindings.mode_json`, `midi_bindings.target_override_json` and
-- `midi_modifiers.groups_json`.
--
-- Apply after every client runs a build without these columns. An older
-- client still uploads them, and Postgres rejects a write to a column that
-- does not exist. `deploy/powersync/sync-config.yaml` selects `*` from these tables, so
-- the sync rules need no change.
--
-- Idempotent.

begin;

alter table public.agent_threads drop column if exists implementation_id;
alter table public.midi_bindings drop column if exists mode_json;
alter table public.midi_bindings drop column if exists target_override_json;
alter table public.midi_modifiers drop column if exists groups_json;

commit;

notify pgrst, 'reload schema';
