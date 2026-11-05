-- A Perform pattern's pad moves into its graph: it is the button node its
-- output's `alpha` depends on, played by that node's settings, so
-- `perform_patterns.pad_json` goes. A pattern whose graph has no such button
-- plays at its level whenever the set is open.
--
-- deploy/powersync/sync-config.yaml selects `*`, so the sync rules need no
-- change.
--
-- Deploy order: ship the client first. A client from before this change
-- uploads `pad_json` in every Perform pattern write, and PostgREST rejects a
-- write naming a column that does not exist. The new client does not send
-- it, and the column's default covers its inserts until this runs.
--
-- Symmetric with backend/migrations/20261105120000_perform_pad_in_graph.sql.
--
-- Idempotent.

begin;

alter table public.perform_patterns drop column if exists pad_json;

commit;

notify pgrst, 'reload schema';
