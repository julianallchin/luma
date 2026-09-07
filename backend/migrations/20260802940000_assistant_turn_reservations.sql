-- Every newly persisted assistant response is one authored turn. The exact
-- score/graph tree is reserved first in authored_state_turn_commits, then the
-- append-only transcript may claim that message identity. Existing legacy
-- rows are intentionally untouched; this closes the insertion path going
-- forward without inventing authored history for old conversations.
CREATE TRIGGER assistant_message_requires_prepared_authored_turn
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN NEW.role = 'assistant'
 AND NOT EXISTS (
    SELECT 1
    FROM authored_state_turn_commits AS authored_turn
    JOIN agent_threads AS thread
      ON thread.id = authored_turn.thread_id
    WHERE authored_turn.thread_id = NEW.thread_id
      AND authored_turn.assistant_message_id = NEW.id
      AND authored_turn.status = 'prepared'
      AND thread.lifecycle_state = 'active'
 )
BEGIN
    SELECT RAISE(ABORT, 'assistant message requires a prepared authored turn');
END;
