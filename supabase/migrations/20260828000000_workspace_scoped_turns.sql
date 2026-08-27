-- Mirror of the local columns added by
-- `src-tauri/migrations/20260828000000_workspace_scoped_turns.sql`.
--
-- `workspace_id` names the private head a subagent turn prepared against. The
-- workspace itself is a local disposable directory that never syncs, so this
-- is a label rather than a reference: what it buys remotely is that another
-- device can tell a workspace-scoped preparation from a live one and decline
-- to finalize it against its own live head.
--
-- `parent_thread_id` / `parent_call_id` carry the subagent relationship, kept
-- reference-free for the same reason `forked_from_thread_id` is: thread rows
-- are mutable, so a parent updated after its child was created is pulled
-- second, and a constraint here would reject the child.
--
-- No backfill: every existing row predates subagent threads, and NULL is the
-- correct answer for all of them. The immutability guard on
-- `authored_turn_preparations` therefore stays armed — an exact-replay upsert
-- of an old preparation sends NULL and still compares identical.

ALTER TABLE public.authored_turn_preparations ADD COLUMN workspace_id text;

ALTER TABLE public.agent_threads ADD COLUMN parent_thread_id text;
ALTER TABLE public.agent_threads ADD COLUMN parent_call_id text;
