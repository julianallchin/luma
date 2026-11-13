-- What a conversation cost becomes part of the record: `agent_thread_usage`
-- syncs, as a source row (docs/design/data-layer.md). A source row has an id
-- of its own and an owner, which this table, local until now, had neither
-- of. It is still found by its thread, one row a thread.
--
-- The rows already here get an id and their thread's owner. They were never
-- on the server, so this is not a rewrite of synced rows; each uploads the
-- next time its conversation runs.
--
-- Symmetric with supabase/migrations/20261113130000_agent_thread_usage.sql.
ALTER TABLE agent_thread_usage ADD COLUMN id TEXT;
ALTER TABLE agent_thread_usage ADD COLUMN uid TEXT;
UPDATE agent_thread_usage SET
    id = lower(
        hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || substr(hex(randomblob(2)), 2)
        || '-' || substr('89ab', 1 + (abs(random()) % 4), 1) || substr(hex(randomblob(2)), 2)
        || '-' || hex(randomblob(6))),
    uid = (SELECT thread.uid FROM agent_threads thread WHERE thread.id = agent_thread_usage.thread_id);
CREATE UNIQUE INDEX idx_agent_thread_usage_id ON agent_thread_usage(id);
