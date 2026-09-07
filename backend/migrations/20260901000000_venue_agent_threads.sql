-- Venue-scoped agent threads.
--
-- A venue thread is about the room: one venue, no track, no score, and no
-- authored document. The rig it revises lives in `venue_nodes`/`venue_edges`,
-- which have no revision history to stage a turn against, so its assistant
-- rows carry no `authored_turn_preparations` row.
--
-- Two triggers therefore move, and one deliberately does not:
--
--   * the route validators gain the third route, so the durable row can be
--     written at all;
--   * `assistant_message_requires_prepared_authored_turn` learns that the
--     invariant is one preparation per assistant row *of a document thread*;
--   * `authored_turn_preparation_requires_admitted_scope` is left alone. It
--     admits a preparation only when the thread's `agent_kind` matches a
--     document kind, and `venue_rig` matches neither branch — so it already
--     refuses to admit a preparation for a venue thread, which is exactly the
--     rule wanted. Widening it would have created the very row the trigger
--     above now says must not exist.

DROP TRIGGER agent_threads_validate_authored_route_insert;
CREATE TRIGGER agent_threads_validate_authored_route_insert
BEFORE INSERT ON agent_threads
WHEN NOT (
    (
        NEW.agent_kind = 'track_copilot'
        AND NEW.subject_kind IS 'track'
        AND NEW.subject_id IS NOT NULL AND NEW.subject_id != ''
        AND NEW.venue_id IS NOT NULL AND NEW.venue_id != ''
        AND NEW.score_id IS NOT NULL AND NEW.score_id != ''
        AND NEW.implementation_id IS NULL
    )
    OR
    (
        NEW.agent_kind = 'pattern_graph'
        AND NEW.subject_kind IS 'pattern'
        AND NEW.subject_id IS NOT NULL AND NEW.subject_id != ''
        AND NEW.implementation_id IS NOT NULL AND NEW.implementation_id != ''
        AND NEW.score_id IS NULL
        AND (NEW.venue_id IS NULL OR NEW.venue_id != '')
        AND (
            EXISTS (
                SELECT 1 FROM implementations
                WHERE id = NEW.implementation_id AND pattern_id = NEW.subject_id
            )
            OR EXISTS (
                SELECT 1 FROM auth_write_admission AS admission
                WHERE admission.singleton = 1 AND admission.remote_writes = 1
                  AND admission.active_uid IS NEW.owner_user_id
            )
        )
    )
    OR
    (
        -- The venue is both the subject and the scope column, so every listing
        -- that filters on `venue_id` sees the thread without a second rule.
        NEW.agent_kind = 'venue_rig'
        AND NEW.subject_kind IS 'venue'
        AND NEW.subject_id IS NOT NULL AND NEW.subject_id != ''
        AND NEW.venue_id IS NEW.subject_id
        AND NEW.score_id IS NULL
        AND NEW.implementation_id IS NULL
    )
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread requires an exact track, pattern, or venue route');
END;

DROP TRIGGER agent_threads_validate_authored_route_update;
CREATE TRIGGER agent_threads_validate_authored_route_update
BEFORE UPDATE OF agent_kind, subject_kind, subject_id, implementation_id, venue_id, score_id ON agent_threads
WHEN NOT (
    (
        NEW.agent_kind = 'track_copilot'
        AND NEW.subject_kind IS 'track'
        AND NEW.subject_id IS NOT NULL AND NEW.subject_id != ''
        AND NEW.venue_id IS NOT NULL AND NEW.venue_id != ''
        AND NEW.score_id IS NOT NULL AND NEW.score_id != ''
        AND NEW.implementation_id IS NULL
    )
    OR
    (
        NEW.agent_kind = 'pattern_graph'
        AND NEW.subject_kind IS 'pattern'
        AND NEW.subject_id IS NOT NULL AND NEW.subject_id != ''
        AND NEW.implementation_id IS NOT NULL AND NEW.implementation_id != ''
        AND NEW.score_id IS NULL
        AND (NEW.venue_id IS NULL OR NEW.venue_id != '')
        AND (
            EXISTS (
                SELECT 1 FROM implementations
                WHERE id = NEW.implementation_id AND pattern_id = NEW.subject_id
            )
            OR EXISTS (
                SELECT 1 FROM auth_write_admission AS admission
                WHERE admission.singleton = 1 AND admission.remote_writes = 1
                  AND admission.active_uid IS NEW.owner_user_id
            )
        )
    )
    OR
    (
        NEW.agent_kind = 'venue_rig'
        AND NEW.subject_kind IS 'venue'
        AND NEW.subject_id IS NOT NULL AND NEW.subject_id != ''
        AND NEW.venue_id IS NEW.subject_id
        AND NEW.score_id IS NULL
        AND NEW.implementation_id IS NULL
    )
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread requires an exact track, pattern, or venue route');
END;

-- One preparation per assistant row of a *document* thread. A venue thread has
-- no document, so requiring one would make its rows unwritable rather than
-- making anything safer.
DROP TRIGGER assistant_message_requires_prepared_authored_turn;
CREATE TRIGGER assistant_message_requires_prepared_authored_turn
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN NEW.role = 'assistant'
 AND NOT EXISTS (
    SELECT 1 FROM agent_threads AS thread
    WHERE thread.id = NEW.created_in_thread_id
      AND thread.agent_kind = 'venue_rig'
 )
 AND NOT EXISTS (
    SELECT 1
    FROM authored_turn_preparations AS authored_turn
    CROSS JOIN auth_write_admission AS admission
    WHERE authored_turn.thread_id = NEW.created_in_thread_id
      AND authored_turn.assistant_message_id = NEW.id
      AND authored_turn.owner_user_id IS NEW.owner_user_id
      AND authored_turn.principal_key = NEW.principal_key
      AND admission.singleton = 1
      AND admission.active_uid IS NEW.owner_user_id
      AND (
          admission.remote_writes = 1
          OR EXISTS (
              SELECT 1 FROM agent_threads AS thread
              WHERE thread.id = authored_turn.thread_id
                AND thread.owner_user_id IS NEW.owner_user_id
                AND thread.lifecycle_state = 'active'
          )
      )
 )
BEGIN
    SELECT RAISE(ABORT, 'assistant message requires a prepared authored turn');
END;

-- A deletion receipt names the scope the thread wrote to. A venue thread wrote
-- to no authored document, so `document_id` becomes nullable rather than being
-- filled with a venue id under a column name that promises otherwise. SQLite
-- cannot drop NOT NULL in place, so the table is rebuilt; the rows are carried
-- across, because a receipt is what makes an interrupted deletion replayable.
DROP TRIGGER agent_thread_deletion_receipt_requires_terminal_scope;
DROP TRIGGER agent_thread_deletion_receipt_is_immutable;
DROP TRIGGER agent_thread_deletion_receipt_is_permanent;
DROP INDEX idx_agent_thread_deletions_owner;
ALTER TABLE agent_thread_deletions RENAME TO agent_thread_deletions_pre_venue;

CREATE TABLE agent_thread_deletions (
    thread_id      TEXT PRIMARY KEY,
    owner_user_id  TEXT,
    principal_key  TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    -- The authored document this thread revised, or NULL for a venue thread,
    -- which revised the room's relational rig instead.
    document_id    TEXT,
    deleted_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

INSERT INTO agent_thread_deletions
    (thread_id, owner_user_id, principal_key, document_id, deleted_at)
SELECT thread_id, owner_user_id, principal_key, document_id, deleted_at
  FROM agent_thread_deletions_pre_venue;

DROP TABLE agent_thread_deletions_pre_venue;

CREATE INDEX idx_agent_thread_deletions_owner
    ON agent_thread_deletions(owner_user_id, deleted_at DESC);

-- Deletion receipts survive their routing row. A document thread must still
-- name its exact authored document; a venue thread must name none.
CREATE TRIGGER agent_thread_deletion_receipt_requires_terminal_scope
BEFORE INSERT ON agent_thread_deletions FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1
    FROM auth_write_admission AS admission
    LEFT JOIN authored_documents AS document
      ON document.document_id = NEW.document_id
     AND document.principal_key = NEW.principal_key
    WHERE admission.singleton = 1
      AND admission.armed = 1
      AND admission.accepting = 1
      AND admission.maintenance = 0
      AND admission.active_uid IS NEW.owner_user_id
      AND NEW.principal_key = CASE
            WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || NEW.owner_user_id
          END
      AND (NEW.document_id IS NULL) = (document.document_id IS NULL)
      AND (
          (
              admission.remote_writes = 1
              AND (
                  NOT EXISTS (
                      SELECT 1 FROM agent_threads AS thread
                      WHERE thread.id = NEW.thread_id
                  )
                  OR EXISTS (
                      SELECT 1 FROM agent_threads AS thread
                      WHERE thread.id = NEW.thread_id
                        AND thread.owner_user_id IS NEW.owner_user_id
                        AND (
                            (
                                thread.agent_kind = 'track_copilot'
                                AND document.document_kind = 'track_score'
                                AND document.track_id = thread.subject_id
                                AND document.venue_id = thread.venue_id
                                AND document.score_id = thread.score_id
                            )
                            OR (
                                thread.agent_kind = 'pattern_graph'
                                AND document.document_kind = 'pattern_graph'
                                AND document.subject_id = thread.subject_id
                                AND document.implementation_id = thread.implementation_id
                            )
                            OR (
                                thread.agent_kind = 'venue_rig'
                                AND NEW.document_id IS NULL
                            )
                        )
                  )
              )
          )
          OR EXISTS (
              SELECT 1 FROM agent_threads AS thread
              WHERE admission.remote_writes = 0
                AND thread.id = NEW.thread_id
                AND thread.owner_user_id IS NEW.owner_user_id
                AND thread.lifecycle_state = 'deleting'
                AND (
                    (
                        thread.agent_kind = 'track_copilot'
                        AND document.document_kind = 'track_score'
                        AND document.track_id = thread.subject_id
                        AND document.venue_id = thread.venue_id
                        AND document.score_id = thread.score_id
                    )
                    OR (
                        thread.agent_kind = 'pattern_graph'
                        AND document.document_kind = 'pattern_graph'
                        AND document.subject_id = thread.subject_id
                        AND document.implementation_id = thread.implementation_id
                    )
                    OR (
                        thread.agent_kind = 'venue_rig'
                        AND NEW.document_id IS NULL
                    )
                )
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
