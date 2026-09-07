-- StateDb stores credentials; the app database's admission row is the sole
-- live authority for durable conversation state. These guards are an
-- unconditional backstop for every caller, including remote/maintenance SQL.

CREATE TRIGGER agent_thread_insert_requires_active_admission
BEFORE INSERT ON agent_threads FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM auth_write_admission admission
    WHERE admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND NEW.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent thread write lacks active principal admission'); END;

CREATE TRIGGER agent_thread_routing_identity_is_immutable
BEFORE UPDATE ON agent_threads FOR EACH ROW
WHEN NEW.id IS NOT OLD.id
  OR NEW.owner_user_id IS NOT OLD.owner_user_id
  OR NEW.agent_kind IS NOT OLD.agent_kind
  OR NEW.subject_kind IS NOT OLD.subject_kind
  OR NEW.subject_id IS NOT OLD.subject_id
  OR NEW.implementation_id IS NOT OLD.implementation_id
  OR NEW.venue_id IS NOT OLD.venue_id
  OR NEW.score_id IS NOT OLD.score_id
  OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'agent thread routing identity is immutable'); END;

CREATE TRIGGER agent_thread_update_requires_active_admission
BEFORE UPDATE ON agent_threads FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM auth_write_admission admission
    WHERE admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND OLD.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent thread write lacks active principal admission'); END;

CREATE TRIGGER agent_thread_delete_requires_active_admission
BEFORE DELETE ON agent_threads FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM auth_write_admission admission
    WHERE admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND OLD.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent thread write lacks active principal admission'); END;

CREATE TRIGGER agent_thread_message_insert_requires_owner_admission
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = NEW.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent thread child write lacks owner admission'); END;

CREATE TRIGGER agent_thread_message_delete_requires_owner_admission
BEFORE DELETE ON agent_thread_messages FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM agent_threads WHERE id = OLD.thread_id
)
AND NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = OLD.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent thread child write lacks owner admission'); END;

CREATE TRIGGER agent_thread_append_insert_requires_owner_admission
BEFORE INSERT ON agent_thread_message_appends FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = NEW.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent thread child write lacks owner admission'); END;

CREATE TRIGGER agent_thread_append_delete_requires_owner_admission
BEFORE DELETE ON agent_thread_message_appends FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM agent_threads WHERE id = OLD.thread_id
)
AND NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = OLD.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent thread child write lacks owner admission'); END;

CREATE TRIGGER authored_thread_route_identity_is_immutable
BEFORE UPDATE ON authored_state_thread_branches FOR EACH ROW
WHEN NEW.thread_id IS NOT OLD.thread_id
  OR NEW.repository_id IS NOT OLD.repository_id
  OR NEW.branch_name IS NOT OLD.branch_name
  OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'authored thread route identity is immutable'); END;

CREATE TRIGGER authored_thread_route_insert_requires_owner_admission
BEFORE INSERT ON authored_state_thread_branches FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = NEW.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'authored thread child write lacks owner admission'); END;

CREATE TRIGGER authored_thread_route_update_requires_owner_admission
BEFORE UPDATE ON authored_state_thread_branches FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = OLD.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'authored thread child write lacks owner admission'); END;

CREATE TRIGGER authored_thread_route_delete_requires_owner_admission
BEFORE DELETE ON authored_state_thread_branches FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = OLD.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'authored thread child write lacks owner admission'); END;

CREATE TRIGGER authored_turn_insert_requires_owner_admission
BEFORE INSERT ON authored_state_turn_commits FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = NEW.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'authored thread child write lacks owner admission'); END;

CREATE TRIGGER authored_turn_update_requires_owner_admission
BEFORE UPDATE ON authored_state_turn_commits FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = OLD.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
OR NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = NEW.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'authored thread child write lacks owner admission'); END;

CREATE TRIGGER authored_turn_delete_requires_owner_admission
BEFORE DELETE ON authored_state_turn_commits FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = OLD.thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'authored thread child write lacks owner admission'); END;

CREATE TRIGGER authored_worktree_insert_requires_owner_admission
BEFORE INSERT ON authored_state_worktrees FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = NEW.owner_thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'authored thread child write lacks owner admission'); END;

CREATE TRIGGER authored_worktree_update_requires_owner_admission
BEFORE UPDATE ON authored_state_worktrees FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = OLD.owner_thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
OR NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = NEW.owner_thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'authored thread child write lacks owner admission'); END;

CREATE TRIGGER authored_worktree_delete_requires_owner_admission
BEFORE DELETE ON authored_state_worktrees FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads thread
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = OLD.owner_thread_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND admission.remote_writes = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'authored thread child write lacks owner admission'); END;
