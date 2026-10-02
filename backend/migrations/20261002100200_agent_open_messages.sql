-- The assistant row a running turn is still writing. A turn used to rewrite
-- its synced `agent_thread_messages` row at every step, and sync stored and
-- shipped every version. It now keeps the row in progress here at each step
-- and writes the synced row once, when the row is finished.
--
-- Local only, one row per thread: a turn writes one assistant row at a time.
-- `parent_message_id` is the transcript head the row is appended at. A row
-- left here by a quit is written to its synced row at the next launch.
CREATE TABLE agent_open_messages (
    thread_id TEXT PRIMARY KEY NOT NULL,
    uid TEXT,
    message_id TEXT NOT NULL,
    parent_message_id TEXT,
    role TEXT NOT NULL,
    parts_json TEXT NOT NULL
);
