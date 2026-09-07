-- Immutable conversation traces outlive the mutable thread lifecycle row.
-- Trusted pull transactions must therefore be able to hydrate a message,
-- append receipt, turn trace, or deletion receipt after another device has
-- already deleted the thread. Local writes retain the stricter active/terminal
-- lifecycle checks; `remote_writes` only relaxes lifecycle existence, never
-- principal binding, message ancestry, append ranges, or row immutability.

-- Preserve server-authored projection clocks. Ordinary local mutations still
-- receive a host timestamp, but a trusted pull must not turn the server value
-- into a new client-clock value merely by materializing it.

-- A locally requested archive timestamp is terminal with respect to state,
-- but another device may have won the server race with a different canonical
-- timestamp. Trusted pull may converge non-null to non-null; neither local nor
-- remote writes may ever resurrect the document by clearing the value.
DROP TRIGGER authored_document_archive_is_terminal;
CREATE TRIGGER authored_document_archive_is_terminal
BEFORE UPDATE OF archived_at ON authored_documents FOR EACH ROW
WHEN OLD.archived_at IS NOT NULL
 AND (
     NEW.archived_at IS NULL
     OR (
         NEW.archived_at IS NOT OLD.archived_at
         AND (SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0
     )
 )
BEGIN
    SELECT RAISE(ABORT, 'archived authored document cannot be restored');
END;

DROP TRIGGER agent_threads_updated_at;
CREATE TRIGGER agent_threads_updated_at
AFTER UPDATE ON agent_threads FOR EACH ROW
WHEN (SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0
BEGIN
    UPDATE agent_threads
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
    WHERE id = OLD.id;
END;

-- Local authored writes remain strict one-generation CAS advances. The
-- pulled server projection is a different operation: it may jump generations
-- or switch branches after deterministic integration, and carries the
-- server's updated_at verbatim.
DROP TRIGGER authored_document_head_is_strict_cas_counter;
CREATE TRIGGER authored_document_head_is_strict_cas_counter
BEFORE UPDATE OF revision_id, generation ON authored_document_heads FOR EACH ROW
WHEN (SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0
 AND (NEW.revision_id IS OLD.revision_id OR NEW.generation <> OLD.generation + 1)
BEGIN
    SELECT RAISE(ABORT, 'authored document head advance must increment generation once');
END;

DROP TRIGGER authored_document_head_updated_at;
CREATE TRIGGER authored_document_head_updated_at
AFTER UPDATE OF revision_id, generation ON authored_document_heads FOR EACH ROW
WHEN (SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0
BEGIN
    UPDATE authored_document_heads
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE document_id = OLD.document_id;
END;

DROP TRIGGER agent_thread_deletion_receipt_requires_terminal_scope;
CREATE TRIGGER agent_thread_deletion_receipt_requires_terminal_scope
BEFORE INSERT ON agent_thread_deletions FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1
      AND admission.armed = 1
      AND admission.accepting = 1
      AND admission.maintenance = 0
      AND admission.active_uid IS NEW.owner_user_id
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
                AND thread.lifecycle_state = 'deleting'
                AND (
                    (
                        thread.agent_kind = 'track_copilot'
                        AND document.document_kind = 'track_score'
                        AND document.track_id = thread.subject_id
                        AND document.venue_id = thread.venue_id
                        AND document.score_id = thread.score_id
                    )
                    OR
                    (
                        thread.agent_kind = 'pattern_graph'
                        AND document.document_kind = 'pattern_graph'
                        AND document.subject_id = thread.subject_id
                        AND document.implementation_id = thread.implementation_id
                    )
                )
          )
      )
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread deletion receipt lacks terminal scope admission');
END;

-- Proposal and integration rows are immutable history, including proposals
-- the server cancelled when an archive won. Local creation still requires a
-- live document; trusted pull may hydrate the retained trace after the parent
-- document's terminal timestamp arrived earlier in topological order.
DROP TRIGGER authored_head_proposal_principal_matches_document;
CREATE TRIGGER authored_head_proposal_principal_matches_document
BEFORE INSERT ON authored_head_proposals FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_id = NEW.document_id
      AND document.principal_key = NEW.principal_key
)
OR (
    COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
    AND EXISTS (
        SELECT 1 FROM authored_documents document
        WHERE document.document_id = NEW.document_id
          AND document.principal_key = NEW.principal_key
          AND document.archived_at IS NOT NULL
    )
)
BEGIN
    SELECT RAISE(ABORT, 'authored proposal principal does not own active document');
END;

-- Submission receipts enrich one otherwise immutable local proposal with its
-- server sequence. Pull may then replay that same row indefinitely. Only a
-- trusted remote-write transaction may perform the NULL -> value transition
-- or an exact value -> same-value replay; local updates, clearing a sequence,
-- and rebinding one server sequence to another remain impossible.
DROP TRIGGER authored_head_proposal_identity_is_immutable;
CREATE TRIGGER authored_head_proposal_identity_is_immutable
BEFORE UPDATE ON authored_head_proposals FOR EACH ROW
WHEN NEW.proposal_id IS NOT OLD.proposal_id
  OR NEW.principal_key IS NOT OLD.principal_key
  OR NEW.document_id IS NOT OLD.document_id
  OR NEW.device_id IS NOT OLD.device_id
  OR NEW.operation_id IS NOT OLD.operation_id
  OR NEW.base_revision_id IS NOT OLD.base_revision_id
  OR NEW.proposed_revision_id IS NOT OLD.proposed_revision_id
  OR NEW.created_at IS NOT OLD.created_at
  OR COALESCE((SELECT remote_writes FROM auth_write_admission WHERE singleton = 1), 0) = 0
  OR NEW.server_proposal_seq IS NULL
  OR (
      OLD.server_proposal_seq IS NOT NULL
      AND NEW.server_proposal_seq IS NOT OLD.server_proposal_seq
  )
BEGIN
    SELECT RAISE(ABORT, 'authored head proposal is immutable');
END;

DROP TRIGGER authored_turn_message_id_cannot_be_reused;
CREATE TRIGGER authored_turn_message_id_cannot_be_reused
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM authored_turn_preparations
    WHERE assistant_message_id = NEW.id
)
AND NOT (
    NEW.role = 'assistant'
    AND EXISTS (
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
)
BEGIN
    SELECT RAISE(ABORT, 'authored turn message identity is immutable');
END;

DROP TRIGGER assistant_message_requires_prepared_authored_turn;
CREATE TRIGGER assistant_message_requires_prepared_authored_turn
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN NEW.role = 'assistant'
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

-- A server receipt may enrich a locally-created archive once, and a later
-- topological pull may fill its canonical final revision after that revision
-- closure arrives. Identity, a non-null final revision, and the server
-- sequence remain immutable. No ordinary local UPDATE is admitted.
DROP TRIGGER authored_document_archive_is_immutable;
CREATE TRIGGER authored_document_archive_is_immutable
BEFORE UPDATE ON authored_document_archives FOR EACH ROW
WHEN NEW.archive_id IS NOT OLD.archive_id
  OR NEW.principal_key IS NOT OLD.principal_key
  OR NEW.document_id IS NOT OLD.document_id
  OR NEW.device_id IS NOT OLD.device_id
  OR NEW.operation_id IS NOT OLD.operation_id
  OR NEW.requested_revision_id IS NOT OLD.requested_revision_id
  OR NEW.archived_at IS NOT OLD.archived_at
  OR (SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) != 1
  OR NEW.server_archive_seq IS NULL
  OR (OLD.server_archive_seq IS NOT NULL
      AND NEW.server_archive_seq IS NOT OLD.server_archive_seq)
  OR (OLD.final_revision_id IS NOT NULL
      AND NEW.final_revision_id IS NOT OLD.final_revision_id)
BEGIN
    SELECT RAISE(ABORT, 'authored document archive is immutable');
END;

-- A pulled transcript node may arrive after its origin thread was deleted.
-- Missing origin is therefore legal only for trusted pull; an existing row
-- must still have the exact owner, and ancestry is never relaxed.
DROP TRIGGER agent_thread_message_requires_valid_parent;
CREATE TRIGGER agent_thread_message_requires_valid_parent
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM agent_threads AS thread
    WHERE thread.id = NEW.created_in_thread_id
      AND thread.owner_user_id IS NOT NEW.owner_user_id
)
OR (
    NEW.parent_message_id IS NULL AND NEW.depth != 0
)
OR (
    NEW.parent_message_id IS NOT NULL
    AND NOT EXISTS (
        SELECT 1 FROM agent_thread_messages AS parent
        WHERE parent.id = NEW.parent_message_id
          AND parent.owner_user_id IS NEW.owner_user_id
          AND parent.principal_key = NEW.principal_key
          AND parent.depth + 1 = NEW.depth
    )
)
BEGIN
    SELECT RAISE(ABORT, 'agent transcript message has an invalid origin, parent, or principal');
END;

DROP TRIGGER agent_thread_message_insert_requires_owner_admission;
CREATE TRIGGER agent_thread_message_insert_requires_owner_admission
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
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
          (
              admission.remote_writes = 1
              AND NOT EXISTS (
                  SELECT 1 FROM agent_threads AS thread
                  WHERE thread.id = NEW.created_in_thread_id
                    AND thread.owner_user_id IS NOT NEW.owner_user_id
              )
          )
          OR (
              admission.remote_writes = 0
              AND EXISTS (
                  SELECT 1 FROM agent_threads AS thread
                  WHERE thread.id = NEW.created_in_thread_id
                    AND thread.owner_user_id IS NEW.owner_user_id
                    AND thread.lifecycle_state = 'active'
              )
          )
      )
)
BEGIN SELECT RAISE(ABORT, 'agent thread child write lacks owner admission'); END;

-- Append receipts retain their exact contiguous new-message range after the
-- lifecycle row disappears. The shared base may come from a fork source, but
-- every newly appended node belongs to this receipt's target thread.
DROP TRIGGER agent_thread_append_has_valid_range;
CREATE TRIGGER agent_thread_append_has_valid_range
BEFORE INSERT ON agent_thread_message_appends FOR EACH ROW
WHEN (
    NEW.base_head_message_id IS NOT NULL
    AND NOT EXISTS (
        SELECT 1 FROM agent_thread_messages AS base
        WHERE base.id = NEW.base_head_message_id
          AND base.owner_user_id IS NEW.owner_user_id
          AND base.principal_key = NEW.principal_key
    )
)
OR NOT EXISTS (
    WITH RECURSIVE lineage(id, parent_message_id) AS (
        SELECT result.id, result.parent_message_id
        FROM agent_thread_messages AS result
        WHERE result.id = NEW.result_head_message_id
          AND result.owner_user_id IS NEW.owner_user_id
          AND result.principal_key = NEW.principal_key
          AND result.created_in_thread_id = NEW.thread_id
        UNION ALL
        SELECT parent.id, parent.parent_message_id
        FROM agent_thread_messages AS parent
        JOIN lineage AS child ON child.parent_message_id = parent.id
        WHERE parent.owner_user_id IS NEW.owner_user_id
          AND parent.principal_key = NEW.principal_key
          AND parent.created_in_thread_id = NEW.thread_id
    )
    SELECT 1
    FROM lineage
    JOIN agent_thread_messages AS first ON first.id = NEW.first_message_id
    JOIN agent_thread_messages AS result ON result.id = NEW.result_head_message_id
    WHERE lineage.id = first.id
      AND first.owner_user_id IS NEW.owner_user_id
      AND first.principal_key = NEW.principal_key
      AND first.created_in_thread_id = NEW.thread_id
      AND first.parent_message_id IS NEW.base_head_message_id
      AND result.depth - first.depth + 1 = NEW.message_count
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread append receipt has invalid scope or range');
END;

DROP TRIGGER agent_thread_append_insert_requires_owner_admission;
CREATE TRIGGER agent_thread_append_insert_requires_owner_admission
BEFORE INSERT ON agent_thread_message_appends FOR EACH ROW
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
          (
              admission.remote_writes = 1
              AND NOT EXISTS (
                  SELECT 1 FROM agent_threads AS thread
                  WHERE thread.id = NEW.thread_id
                    AND thread.owner_user_id IS NOT NEW.owner_user_id
              )
          )
          OR (
              admission.remote_writes = 0
              AND EXISTS (
                  SELECT 1 FROM agent_threads AS thread
                  WHERE thread.id = NEW.thread_id
                    AND thread.owner_user_id IS NEW.owner_user_id
                    AND thread.lifecycle_state = 'active'
              )
          )
      )
)
BEGIN SELECT RAISE(ABORT, 'agent thread child write lacks owner admission'); END;

-- A preparation always proves its exact codec revision. Trusted pull may
-- hydrate it after thread deletion, but if the routing row still exists it
-- must match the same owner and authored-document route.
DROP TRIGGER authored_turn_preparation_requires_admitted_scope;
CREATE TRIGGER authored_turn_preparation_requires_admitted_scope
BEFORE INSERT ON authored_turn_preparations FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1
    FROM auth_write_admission AS admission
    JOIN authored_documents AS document
      ON document.document_id = NEW.document_id
     AND document.principal_key = NEW.principal_key
    JOIN authored_revisions AS revision
      ON revision.revision_id = NEW.prepared_revision_id
     AND revision.document_id = NEW.document_id
     AND revision.principal_key = NEW.principal_key
    WHERE admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND NEW.owner_user_id IS admission.active_uid
      AND NEW.principal_key = CASE
            WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || NEW.owner_user_id
          END
      AND revision.operation_kind = 'agent_turn_prepare'
      AND revision.operation_id = NEW.assistant_message_id
      AND revision.thread_id = NEW.thread_id
      AND revision.assistant_message_id IS NULL
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
                        )
                  )
              )
          )
          OR (
              admission.remote_writes = 0
              AND document.archived_at IS NULL
              AND EXISTS (
                  SELECT 1 FROM agent_threads AS thread
                  WHERE thread.id = NEW.thread_id
                    AND thread.owner_user_id IS NEW.owner_user_id
                    AND thread.lifecycle_state = 'active'
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
                    )
              )
          )
      )
)
BEGIN SELECT RAISE(ABORT, 'authored turn preparation lacks admitted revision scope'); END;

DROP TRIGGER authored_turn_outcome_matches_persisted_assistant;
CREATE TRIGGER authored_turn_outcome_matches_persisted_assistant
BEFORE INSERT ON authored_turn_outcomes FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1
    FROM authored_turn_preparations AS preparation
    JOIN agent_thread_messages AS message
      ON message.id = NEW.assistant_message_id
    WHERE preparation.thread_id = NEW.thread_id
      AND preparation.assistant_message_id = NEW.assistant_message_id
      AND preparation.owner_user_id IS NEW.owner_user_id
      AND preparation.principal_key = NEW.principal_key
      AND preparation.document_id = NEW.document_id
      AND preparation.prepared_revision_id = NEW.prepared_revision_id
      AND message.created_in_thread_id = NEW.thread_id
      AND message.owner_user_id IS NEW.owner_user_id
      AND message.principal_key = NEW.principal_key
      AND message.role = 'assistant'
      AND (
          NEW.status = 'conflicted'
          OR EXISTS (
              SELECT 1
              FROM authored_revisions AS result
              WHERE result.revision_id = NEW.result_revision_id
                AND result.document_id = NEW.document_id
                AND result.principal_key = NEW.principal_key
                AND result.operation_kind = 'agent_turn'
                AND result.operation_id = NEW.assistant_message_id
                AND result.thread_id = NEW.thread_id
                AND result.assistant_message_id = NEW.assistant_message_id
                AND EXISTS (
                    SELECT 1 FROM authored_revision_parents AS parent
                    WHERE parent.revision_id = result.revision_id
                      AND parent.parent_order = 1
                      AND parent.parent_revision_id = NEW.prepared_revision_id
                )
          )
      )
)
BEGIN SELECT RAISE(ABORT, 'authored turn outcome lacks its persisted assistant revision'); END;

DROP TRIGGER authored_turn_outcome_insert_requires_owner_admission;
CREATE TRIGGER authored_turn_outcome_insert_requires_owner_admission
BEFORE INSERT ON authored_turn_outcomes FOR EACH ROW
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
          (
              admission.remote_writes = 1
              AND NOT EXISTS (
                  SELECT 1 FROM agent_threads AS thread
                  WHERE thread.id = NEW.thread_id
                    AND thread.owner_user_id IS NOT NEW.owner_user_id
              )
          )
          OR (
              admission.remote_writes = 0
              AND EXISTS (
                  SELECT 1 FROM agent_threads AS thread
                  WHERE thread.id = NEW.thread_id
                    AND thread.owner_user_id IS NEW.owner_user_id
                    AND thread.lifecycle_state = 'active'
              )
          )
      )
)
BEGIN SELECT RAISE(ABORT, 'authored turn outcome lacks owner admission'); END;

-- Deletion receipts also survive their routing row. The authored document is
-- always required; an extant thread must match its owner and exact route.
DROP TRIGGER agent_thread_deletion_receipt_requires_terminal_scope;
CREATE TRIGGER agent_thread_deletion_receipt_requires_terminal_scope
BEFORE INSERT ON agent_thread_deletions FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1
    FROM auth_write_admission AS admission
    JOIN authored_documents AS document
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
                )
          )
      )
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread deletion receipt lacks terminal scope admission');
END;
