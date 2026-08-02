-- One durable replay receipt per atomic transcript append. The receipt points
-- at the immutable message range rather than duplicating message/tool payloads;
-- exact generated IDs and timestamps replay from the message rows themselves.
-- These rows are transcript machinery, not authored history, and leave with
-- their parent thread.
CREATE TABLE agent_thread_message_appends (
    thread_id           TEXT NOT NULL,
    operation_id        TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    first_seq           INTEGER NOT NULL CHECK (first_seq >= 0),
    message_count       INTEGER NOT NULL CHECK (message_count > 0),
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    PRIMARY KEY (thread_id, operation_id),
    FOREIGN KEY (thread_id) REFERENCES agent_threads(id) ON DELETE CASCADE
);

CREATE TRIGGER agent_thread_append_requires_active_thread
BEFORE INSERT ON agent_thread_message_appends FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads
    WHERE id = NEW.thread_id AND lifecycle_state = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread is not active');
END;

-- Reservation precedes assistant persistence. A message that already exists
-- cannot be retroactively claimed by a new turn; an exact same-row retry is
-- allowed so preparation remains recoverable after response loss.
CREATE TRIGGER authored_turn_message_reservation_requires_unused_id
BEFORE INSERT ON authored_state_turn_commits FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM agent_thread_messages
    WHERE id = NEW.assistant_message_id
)
AND NOT EXISTS (
    SELECT 1 FROM authored_state_turn_commits
    WHERE thread_id = NEW.thread_id
      AND assistant_message_id = NEW.assistant_message_id
)
BEGIN
    SELECT RAISE(ABORT, 'authored turn cannot reserve an existing message id');
END;

-- A prepared authored turn reserves its assistant-message ID before the
-- transcript append. Only the matching assistant row on that active thread
-- may claim it; no other role or thread can reuse the audit identity.
CREATE TRIGGER authored_turn_message_id_cannot_be_reused
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN EXISTS (
    SELECT 1
    FROM authored_state_turn_commits
    WHERE assistant_message_id = NEW.id
)
AND NOT (
    NEW.role = 'assistant'
    AND EXISTS (
        SELECT 1
        FROM authored_state_turn_commits AS authored_turn
        JOIN agent_threads AS thread
          ON thread.id = authored_turn.thread_id
        WHERE authored_turn.thread_id = NEW.thread_id
          AND authored_turn.assistant_message_id = NEW.id
          AND thread.lifecycle_state = 'active'
    )
)
BEGIN
    SELECT RAISE(ABORT, 'authored turn message identity is immutable');
END;

-- Persisted messages are an append-only conversation log. Redo is a new
-- turn, and rewind restores authored Git state without changing the log.
-- Updates have no valid lifecycle. Deletes are allowed only after the parent
-- enters terminal `deleting`, so full thread retirement can cascade.
CREATE TRIGGER agent_thread_message_cannot_be_updated
BEFORE UPDATE ON agent_thread_messages FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent thread transcript is append-only');
END;

CREATE TRIGGER active_agent_thread_message_cannot_be_deleted
BEFORE DELETE ON agent_thread_messages FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM agent_threads
    WHERE id = OLD.thread_id AND lifecycle_state = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'active agent thread transcript is append-only');
END;

-- A replay receipt is the permanent identity of the exact committed batch.
CREATE TRIGGER agent_thread_append_receipt_is_immutable
BEFORE UPDATE ON agent_thread_message_appends FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent thread append receipt is immutable');
END;

CREATE TRIGGER active_agent_thread_append_receipt_cannot_be_deleted
BEFORE DELETE ON agent_thread_message_appends FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM agent_threads
    WHERE id = OLD.thread_id AND lifecycle_state = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'active agent thread append receipt is immutable');
END;
