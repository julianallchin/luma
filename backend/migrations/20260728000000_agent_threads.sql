-- Durable agent threads: the persistent conversation record for both the track
-- copilot and the pattern-graph agent. One row per thread, one row per message.
--
-- `parts_json` stores the AI SDK `UIMessage.parts` array verbatim. The part
-- union is open (text, reasoning, tool calls, tool results, file/artifact
-- references, provider-specific parts), so the backend deliberately treats it
-- as opaque JSON: `src/shared/components/agent-chat/parts.ts` stays the single
-- interpreter. Messages are stored row-per-message rather than as one blob so
-- that streaming turns can append incrementally and edit-and-resend is a
-- `DELETE WHERE seq >= ?` rather than a whole-history rewrite.
--
-- DELIBERATE EXCLUSIONS:
--   * NOT registered in `src-tauri/src/sync/registry.rs`. Threads are local to
--     the machine that ran them: a thread owns a Python workspace on local disk
--     (`<app_config>/agent-workspaces/<thread-id>/`) that cannot be synced, and
--     transcripts can be large and contain machine-local paths. Consequently
--     there are no `uid` / `version` / `synced_at` / `origin` columns and the
--     updated_at trigger carries no `WHEN OLD.version = NEW.version` guard.
--   * NOT added to the table list in `commands::auth::wipe_database`. That wipe
--     runs on sign-out to drop the *synced* library of the previous user;
--     agent threads are not part of that dataset and outliving a sign-out is
--     the intended behaviour. Thread deletion is explicit, via
--     `agent_thread_delete`.

CREATE TABLE agent_threads (
    id           TEXT PRIMARY KEY,          -- uuid v4
    agent_kind   TEXT NOT NULL,             -- 'track_copilot' | 'pattern_graph'
    subject_kind TEXT,                      -- 'track' | 'pattern' | NULL
    subject_id   TEXT,
    venue_id     TEXT,
    score_id     TEXT,
    title        TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE INDEX idx_agent_threads_subject ON agent_threads(subject_kind, subject_id);

CREATE TABLE agent_thread_messages (
    id          TEXT PRIMARY KEY,           -- UIMessage.id
    thread_id   TEXT NOT NULL,
    seq         INTEGER NOT NULL,
    role        TEXT NOT NULL,
    parts_json  TEXT NOT NULL,              -- UIMessage.parts verbatim
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    UNIQUE (thread_id, seq),
    FOREIGN KEY (thread_id) REFERENCES agent_threads(id) ON DELETE CASCADE
);

CREATE INDEX idx_agent_thread_messages_thread ON agent_thread_messages(thread_id, seq);

CREATE TRIGGER agent_threads_updated_at AFTER UPDATE ON agent_threads FOR EACH ROW
BEGIN
    UPDATE agent_threads SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = OLD.id;
END;
