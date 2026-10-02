-- Cues and the account pattern library leave the product. Clips play the
-- built-in forms and presets; score-local definitions stay in
-- `score_definitions`.
--
-- Apply after the clip-forms change set, which deletes these rows and clears
-- `agent_threads.implementation_id`, and after `deploy/powersync/sync-config.yaml` no
-- longer names these tables: a sync rule over a dropped table fails its
-- whole stream.
--
-- Idempotent, like the change set before it.

begin;

-- A binding that fired a cue, or blacked cues out, has no action left.
delete from public.midi_bindings
where action_json ~ '"type"\s*:\s*"(fireCue|blackout)"';

drop table if exists public.cues;
drop table if exists public.implementations;
drop table if exists public.patterns;

-- After the tables: their row-level policies call these.
drop function if exists public.can_read_pattern (text);
drop function if exists public.pattern_is_cued (text);

commit;

notify pgrst, 'reload schema';
