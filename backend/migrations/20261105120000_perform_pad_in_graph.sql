-- A Perform pattern's pad moves into its graph. The pad is the button node
-- its output's `alpha` depends on (`ClipGraph::pad`), played by that node's
-- settings, so `pad_json` (a `luma_patterns::press::Press` of its own) has
-- nothing left to say. A pattern whose graph has no such button plays at its
-- level whenever the set is open.
--
-- The matching Postgres change is
-- `supabase/migrations/20261105120000_perform_pad_in_graph.sql`.

ALTER TABLE perform_patterns DROP COLUMN pad_json;
