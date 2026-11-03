-- MIDI modifiers and bindings leave the product. Live control binds MIDI and
-- OSC inputs to a venue's controls, and those bindings are local to each
-- machine (`control_bindings`, a local table), so nothing replaces these on
-- the server. See docs/design/live-control.md.
--
-- Apply after every client runs a build without these tables, and after
-- `deploy/powersync/sync-config.yaml` no longer names them: a sync rule over
-- a dropped table fails its whole stream.
--
-- Idempotent.

begin;

drop trigger if exists midi_modifiers_history on public.midi_modifiers;
drop trigger if exists midi_bindings_history on public.midi_bindings;
drop table if exists public.midi_bindings;
drop table if exists public.midi_modifiers;

commit;

notify pgrst, 'reload schema';
