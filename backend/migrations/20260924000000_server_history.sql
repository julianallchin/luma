-- History moves to the server. A Postgres trigger records every authored row
-- change there (see `supabase/migrations/20260924000000_server_history.sql`),
-- so the local change log and the triggers that filled it go. `changes` no
-- longer syncs, and its rows are dropped with it.
--
-- Its one reader was a score's "last edited" time. That is now a column of
-- its own, written by `save_score` when a save moves something, and synced so
-- every device shows the same time. Existing scores start empty and the
-- listing falls back to `updated_at` until their next edit: a backfill would
-- be an UPDATE on `scores`, which the auth admission triggers refuse in
-- migration context.

DROP TABLE IF EXISTS changes;

ALTER TABLE scores ADD COLUMN authored_at TEXT;
