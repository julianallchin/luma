-- What one agent run against a thread cost.
--
-- One row per thread, absolute rather than incremental: a writer reports the
-- thread's running totals and this table stores the latest report, so recording
-- the same run twice is the same row and not a doubled one.
--
-- DELIBERATELY NO FOREIGN KEY to `agent_threads`. A completed thread deletion
-- removes the lifecycle row (see `20260802900000_agent_thread_deletion_receipts.sql`),
-- and the out-of-process MCP host deletes its thread the moment the client hangs
-- up — which is *before* the harness that spawned it knows what the run cost.
-- So this is a permanent ledger keyed by thread id, exactly like
-- `authored_revisions.thread_id`, which is also FK-free for the same reason and
-- is the column that joins a run's cost back to the score it authored.
--
-- Local only, like the threads it accounts for: not in
-- `src-tauri/src/sync/registry.rs`, not in `wipe_database`. A cost is a fact
-- about this machine's spending, not part of the synced library.
CREATE TABLE agent_thread_usage (
    thread_id             TEXT PRIMARY KEY,
    -- The model that ran, in whatever vocabulary the writer uses: a `ModelId`
    -- key from the in-app loop, the CLI's own model name from a harness.
    model                 TEXT,
    turns                 INTEGER NOT NULL DEFAULT 0,
    -- Anthropic's convention, the one `agent::model::Usage` already documents:
    -- the four counts do not overlap, and their sum is the whole spend.
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    -- NULL when nobody priced the run. Only a writer that is *told* the cost
    -- fills this in; nothing here estimates from a rate card, because a rate
    -- card in the tree is a second source of truth that silently rots.
    cost_usd              REAL,
    duration_ms           INTEGER NOT NULL DEFAULT 0,
    -- How many children the run fanned out to. Zero is "none", not "unknown".
    subagents             INTEGER NOT NULL DEFAULT 0,
    recorded_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
