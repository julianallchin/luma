-- A completed thread deletion removes the lifecycle row which made retries
-- distinguishable from unknown IDs. Preserve one exact-owner terminal receipt
-- in the same transaction as the delete so uncertain responses are safely
-- replayable without rerunning external cleanup.
CREATE TABLE agent_thread_deletions (
    thread_id      TEXT PRIMARY KEY,
    owner_user_id  TEXT,
    repository_id  TEXT NOT NULL,
    deleted_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE INDEX idx_agent_thread_deletions_owner
    ON agent_thread_deletions(owner_user_id, deleted_at DESC);

CREATE TRIGGER agent_thread_deletion_receipt_requires_terminal_scope
BEFORE INSERT ON agent_thread_deletions
FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1
    FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = NEW.thread_id
      AND thread.owner_user_id IS NEW.owner_user_id
      AND thread.lifecycle_state = 'deleting'
      AND (
          EXISTS (
              SELECT 1
              FROM authored_state_thread_branches branch
              WHERE branch.thread_id = NEW.thread_id
                AND branch.repository_id = NEW.repository_id
          )
          -- A thread created before authored routing (or interrupted before
          -- materialization) still needs an idempotent terminal receipt. An
          -- existing route must match exactly; only a genuinely route-less
          -- lifecycle row takes this migration/recovery path.
          OR NOT EXISTS (
              SELECT 1 FROM authored_state_thread_branches branch
              WHERE branch.thread_id = NEW.thread_id
          )
      )
      AND admission.singleton = 1
      AND (
          admission.armed = 0
          OR (
              admission.accepting = 1
              AND admission.maintenance = 0
              AND admission.remote_writes = 0
              AND admission.active_uid IS NEW.owner_user_id
          )
      )
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread deletion receipt lacks terminal scope admission');
END;

CREATE TRIGGER agent_thread_deletion_receipt_is_immutable
BEFORE UPDATE ON agent_thread_deletions
FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent thread deletion receipt is immutable');
END;

CREATE TRIGGER agent_thread_deletion_receipt_is_permanent
BEFORE DELETE ON agent_thread_deletions
FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent thread deletion receipt is permanent');
END;
