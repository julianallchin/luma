-- The score as a thread's agent last left it: one mark per clip, so the next
-- message can say what the user changed by hand since. Local only, like
-- agent_thread_usage: it describes this device's view of a conversation.
CREATE TABLE agent_thread_score_marks (
    thread_id   TEXT PRIMARY KEY REFERENCES agent_threads(id) ON DELETE CASCADE,
    score_id    TEXT NOT NULL,
    marks_json  TEXT NOT NULL CHECK (json_valid(marks_json)),
    recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
