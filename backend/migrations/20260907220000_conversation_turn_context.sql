-- A conversation may work on a different document each turn. Ownership remains
-- enforced; immutable creation metadata no longer limits the working context.
DROP TRIGGER authored_turn_preparation_requires_admitted_scope;
CREATE TRIGGER authored_turn_preparation_requires_admitted_scope
BEFORE INSERT ON authored_turn_preparations FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND NEW.owner_user_id IS admission.active_uid
      AND NEW.principal_key = CASE
            WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || NEW.owner_user_id
          END
      AND (
          admission.remote_writes = 1
          OR EXISTS (
              SELECT 1
              FROM agent_threads AS thread
              JOIN authored_documents AS document
                ON document.document_id = NEW.document_id
               AND document.principal_key = NEW.principal_key
              WHERE thread.id = NEW.thread_id
                AND thread.owner_user_id IS NEW.owner_user_id
                AND thread.lifecycle_state = 'active'
                AND document.archived_at IS NULL

          )
      )
)
BEGIN SELECT RAISE(ABORT, 'authored turn preparation lacks admitted document scope'); END;

DROP TRIGGER assistant_message_requires_prepared_authored_turn;
CREATE TRIGGER assistant_message_requires_prepared_authored_turn
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN NEW.role = 'assistant'
 AND COALESCE((
    WITH RECURSIVE lineage AS (
        SELECT id, parent_message_id, depth, role, parts_json
        FROM agent_thread_messages WHERE id = NEW.parent_message_id
        UNION ALL
        SELECT parent.id, parent.parent_message_id, parent.depth, parent.role, parent.parts_json
        FROM agent_thread_messages parent JOIN lineage child ON parent.id = child.parent_message_id
    )
    SELECT COALESCE(json_extract(part.value, '$.data.scope.agentKind'), 'unbound')
    FROM lineage, json_each(lineage.parts_json) part
    WHERE lineage.role = 'user' AND json_extract(part.value, '$.type') = 'data-luma-context'
    ORDER BY lineage.depth DESC LIMIT 1
 ), (SELECT agent_kind FROM agent_threads WHERE id = NEW.created_in_thread_id))
 IN ('track_copilot', 'pattern_graph')
 AND NOT EXISTS (
    SELECT 1 FROM authored_turn_preparations preparation
    WHERE preparation.assistant_message_id = NEW.id
      AND preparation.thread_id = NEW.created_in_thread_id
      AND preparation.owner_user_id IS NEW.owner_user_id
      AND preparation.principal_key = NEW.principal_key
 )
BEGIN SELECT RAISE(ABORT, 'assistant message requires its immutable turn preparation'); END;

-- The service binds a child's workspace to its parent's current document.
-- Creation metadata may name a different document from an earlier turn.
DROP TRIGGER agent_thread_parent_shares_its_scope;
CREATE TRIGGER agent_thread_parent_shares_its_owner
BEFORE INSERT ON agent_threads FOR EACH ROW
WHEN NEW.parent_thread_id IS NOT NULL AND (
    NEW.parent_thread_id = NEW.id OR EXISTS (
        SELECT 1 FROM agent_threads parent
        WHERE parent.id = NEW.parent_thread_id AND parent.owner_user_id IS NOT NEW.owner_user_id
    )
)
BEGIN SELECT RAISE(ABORT, 'subagent thread must share its parent owner'); END;
