-- The row model renamed `owner_user_id` to `uid` across the agent thread
-- tables (20260912000000_row_model.sql, lines 592-594) but missed this one,
-- while the claim path was updated with the rest. So claiming a thread failed
-- on a column that no longer matched -- "table agent_thread_runs has no column
-- named uid" -- and no message could be sent.
--
-- The table is local only: it is this device's receipts for its own cloud
-- execution claims, maintained through the claim/release RPCs rather than row
-- sync, so this rename is not visible to any other device.
ALTER TABLE agent_thread_runs RENAME COLUMN owner_user_id TO uid;
