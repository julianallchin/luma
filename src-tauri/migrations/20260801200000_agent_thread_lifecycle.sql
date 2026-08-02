-- Thread deletion spans SQLite, Git worktrees, Python workspaces, and the
-- in-memory graph-run store. The durable state transition closes the window in
-- which a thread-owned operation could create a new child after deletion had
-- already enumerated the old ones. `deleting` is intentionally terminal:
-- cleanup retries resume from it, while every normal thread operation sees
-- only `active` rows.
ALTER TABLE agent_threads
    ADD COLUMN lifecycle_state TEXT NOT NULL DEFAULT 'active'
        CHECK (lifecycle_state IN ('active', 'deleting'));

CREATE INDEX idx_agent_threads_owner_lifecycle_updated
    ON agent_threads(owner_user_id, lifecycle_state, updated_at DESC);

CREATE TRIGGER agent_thread_deletion_is_terminal
BEFORE UPDATE OF lifecycle_state ON agent_threads
FOR EACH ROW
WHEN OLD.lifecycle_state = 'deleting' AND NEW.lifecycle_state != 'deleting'
BEGIN
    SELECT RAISE(ABORT, 'deleting agent thread cannot be reactivated');
END;

-- These tables intentionally retain historical rows after a thread is gone,
-- so foreign keys with ON DELETE CASCADE would be the wrong ownership model.
-- Instead, reject only creation/reactivation of live children unless their
-- owning thread is active. Existing children remain available to deletion's
-- retryable cleanup pass.
CREATE TRIGGER authored_thread_branch_requires_active_thread
BEFORE INSERT ON authored_state_thread_branches
FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads
    WHERE id = NEW.thread_id AND lifecycle_state = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'authored thread is not active');
END;

CREATE TRIGGER authored_turn_insert_requires_active_thread
BEFORE INSERT ON authored_state_turn_commits
FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads
    WHERE id = NEW.thread_id AND lifecycle_state = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'authored thread is not active');
END;

CREATE TRIGGER authored_turn_update_requires_active_thread
BEFORE UPDATE ON authored_state_turn_commits
FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads
    WHERE id = NEW.thread_id AND lifecycle_state = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'authored thread is not active');
END;

CREATE TRIGGER authored_worktree_insert_requires_active_thread
BEFORE INSERT ON authored_state_worktrees
FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads
    WHERE id = NEW.owner_thread_id AND lifecycle_state = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'authored thread is not active');
END;

CREATE TRIGGER authored_worktree_activate_requires_active_thread
BEFORE UPDATE OF status ON authored_state_worktrees
FOR EACH ROW
WHEN NEW.status IN ('preparing', 'active') AND NOT EXISTS (
    SELECT 1 FROM agent_threads
    WHERE id = NEW.owner_thread_id AND lifecycle_state = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'authored thread is not active');
END;
