-- A pattern may own several graph implementations. Pattern-agent threads are
-- therefore pinned to one concrete implementation for their entire lifetime;
-- pattern_id alone is presentation/catalog identity, not authored-state
-- identity.
--
-- Legacy repair is lossless and deterministic:
--   * a venue override, unique unnamed default, or sole implementation pins
--     the original thread;
--   * a genuinely ambiguous transcript is copied once per implementation,
--     with injective deterministic thread/message IDs, then the unresolved
--     source row is removed;
--   * a transcript whose synced catalog is absent receives an explicit,
--     deterministic recovery identity without synthesizing user-visible
--     library rows. It remains readable and deletable through stable routing;
--     normal graph access still fails closed until a real scope exists.
ALTER TABLE agent_threads
    ADD COLUMN implementation_id TEXT;

UPDATE agent_threads
SET subject_kind = 'pattern',
    subject_id = COALESCE(
        subject_id,
        'legacy-pattern-' || lower(hex(CAST(id AS BLOB)))
    )
WHERE agent_kind = 'pattern_graph';

UPDATE agent_threads
SET implementation_id = COALESCE(
    (
        SELECT override.implementation_id
        FROM venue_implementation_overrides AS override
        JOIN implementations AS implementation
          ON implementation.id = override.implementation_id
         AND implementation.pattern_id = agent_threads.subject_id
        WHERE override.venue_id = agent_threads.venue_id
          AND override.pattern_id = agent_threads.subject_id
        LIMIT 1
    ),
    (
        SELECT implementation.id
        FROM implementations AS implementation
        WHERE implementation.pattern_id = agent_threads.subject_id
          AND implementation.name IS NULL
          AND 1 = (
              SELECT COUNT(*) FROM implementations AS candidate
              WHERE candidate.pattern_id = agent_threads.subject_id
                AND candidate.name IS NULL
          )
        ORDER BY implementation.created_at, implementation.id
        LIMIT 1
    ),
    (
        SELECT implementation.id
        FROM implementations AS implementation
        WHERE implementation.pattern_id = agent_threads.subject_id
          AND 1 = (
              SELECT COUNT(*) FROM implementations AS candidate
              WHERE candidate.pattern_id = agent_threads.subject_id
          )
        ORDER BY implementation.created_at, implementation.id
        LIMIT 1
    )
)
WHERE agent_kind = 'pattern_graph' AND subject_kind = 'pattern';

-- No retained graph thread may have incomplete routing. This identity is not
-- an implementation row and is never exposed in the pattern catalog; it is a
-- durable route for otherwise unknowable legacy state, including deletion.
UPDATE agent_threads
SET implementation_id =
    'legacy-unmaterialized-' || lower(hex(CAST(subject_id AS BLOB)))
WHERE agent_kind = 'pattern_graph'
  AND subject_kind = 'pattern'
  AND implementation_id IS NULL
  AND NOT EXISTS (
      SELECT 1 FROM implementations AS implementation
      WHERE implementation.pattern_id = agent_threads.subject_id
  );

-- Anything still unresolved while its pattern has implementations is truly
-- ambiguous. Copy the complete thread metadata and transcript into one exact
-- thread per implementation. hex(old-id || NUL || implementation-id) is an
-- injective, ref-safe identity encoding; it neither guesses nor depends on row
-- ordering. Message IDs are remapped because their primary key is global.
INSERT INTO agent_threads (
    id, owner_user_id, agent_kind, subject_kind, subject_id,
    implementation_id, venue_id, score_id, title,
    created_at, updated_at, lifecycle_state
)
SELECT
    'legacy-thread-' || lower(hex(CAST(thread.id || char(0) || implementation.id AS BLOB))),
    thread.owner_user_id,
    thread.agent_kind,
    thread.subject_kind,
    thread.subject_id,
    implementation.id,
    thread.venue_id,
    thread.score_id,
    thread.title,
    thread.created_at,
    thread.updated_at,
    'active'
FROM agent_threads AS thread
JOIN implementations AS implementation
  ON implementation.pattern_id = thread.subject_id
WHERE thread.agent_kind = 'pattern_graph'
  AND thread.subject_kind = 'pattern'
  AND thread.implementation_id IS NULL
  AND thread.lifecycle_state = 'active';

INSERT INTO agent_thread_messages (
    id, thread_id, seq, role, parts_json, created_at
)
SELECT
    'legacy-message-' || lower(hex(CAST(message.id || char(0) || implementation.id AS BLOB))),
    'legacy-thread-' || lower(hex(CAST(thread.id || char(0) || implementation.id AS BLOB))),
    message.seq,
    message.role,
    message.parts_json,
    message.created_at
FROM agent_threads AS thread
JOIN implementations AS implementation
  ON implementation.pattern_id = thread.subject_id
JOIN agent_thread_messages AS message
  ON message.thread_id = thread.id
WHERE thread.agent_kind = 'pattern_graph'
  AND thread.subject_kind = 'pattern'
  AND thread.implementation_id IS NULL
  AND thread.lifecycle_state = 'active';

-- App migrations intentionally run with foreign keys disabled. Delete the
-- migrated source transcript explicitly before its thread row; do not rely on
-- the message table's ON DELETE CASCADE here.
DELETE FROM agent_thread_messages
WHERE thread_id IN (
    SELECT thread.id
    FROM agent_threads AS thread
    WHERE thread.agent_kind = 'pattern_graph'
      AND thread.subject_kind = 'pattern'
      AND thread.implementation_id IS NULL
      AND thread.lifecycle_state = 'active'
      AND EXISTS (
          SELECT 1 FROM implementations AS implementation
          WHERE implementation.pattern_id = thread.subject_id
      )
);

DELETE FROM agent_threads
WHERE agent_kind = 'pattern_graph'
  AND subject_kind = 'pattern'
  AND implementation_id IS NULL
  AND lifecycle_state = 'active'
  AND EXISTS (
      SELECT 1 FROM implementations AS implementation
      WHERE implementation.pattern_id = agent_threads.subject_id
  );

CREATE INDEX idx_agent_threads_graph_implementation
    ON agent_threads(implementation_id)
    WHERE agent_kind = 'pattern_graph';

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
        AND EXISTS (
            SELECT 1 FROM implementations
            WHERE id = NEW.implementation_id AND pattern_id = NEW.subject_id
        )
    )
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread requires an exact track or pattern authored route');
END;

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
        AND EXISTS (
            SELECT 1 FROM implementations
            WHERE id = NEW.implementation_id AND pattern_id = NEW.subject_id
        )
    )
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread requires an exact track or pattern authored route');
END;
