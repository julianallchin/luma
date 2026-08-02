-- Intentional deletion is a terminal authored-state materialization state.
-- `absent` means sign-out temporarily removed the live projection and may be
-- rematerialized from Git. `archived` means the user (or another synced
-- device) deleted the catalog container; Git refs and history remain, but the
-- live projection must never be recreated implicitly.

-- A stale create retry or remote catalog upsert cannot resurrect a terminally
-- archived authored document. These guards intentionally also cover writes
-- that bypass AuthoredDocuments.
CREATE TRIGGER prevent_archived_score_resurrection
BEFORE INSERT ON scores FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM authored_state_projections
    WHERE document_kind = 'track_score'
      AND score_id = NEW.id
      AND materialization_state = 'archived'
)
BEGIN
    SELECT RAISE(ABORT, 'cannot recreate an archived authored score');
END;

CREATE TRIGGER prevent_archived_pattern_resurrection
BEFORE INSERT ON patterns FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM authored_state_projections
    WHERE document_kind = 'pattern_graph'
      AND subject_id = NEW.id
      AND materialization_state = 'archived'
)
BEGIN
    SELECT RAISE(ABORT, 'cannot recreate an archived authored pattern');
END;

-- Normal deletes must first make the terminal ledger transition in the same
-- transaction. A pattern catalog row with neither an implementation nor a
-- ledger has no authored document to preserve and may be removed directly.
-- Auth maintenance is the other exception: sign-out deliberately removes
-- signed-in catalog rows after marking their ledgers `absent`.
CREATE TRIGGER require_archived_score_before_delete
BEFORE DELETE ON scores FOR EACH ROW
WHEN (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND (
    NOT EXISTS (
        SELECT 1 FROM authored_state_projections
        WHERE document_kind = 'track_score' AND score_id = OLD.id
    )
    OR EXISTS (
        SELECT 1 FROM authored_state_projections
        WHERE document_kind = 'track_score'
          AND score_id = OLD.id
          AND materialization_state != 'archived'
    )
 )
BEGIN
    SELECT RAISE(ABORT, 'score deletion requires an archived authored projection');
END;

CREATE TRIGGER require_archived_pattern_before_delete
BEFORE DELETE ON patterns FOR EACH ROW
WHEN (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND (
    EXISTS (
        SELECT 1 FROM authored_state_projections
        WHERE document_kind = 'pattern_graph'
          AND subject_id = OLD.id
          AND materialization_state != 'archived'
    )
    OR (
        NOT EXISTS (
            SELECT 1 FROM authored_state_projections
            WHERE document_kind = 'pattern_graph' AND subject_id = OLD.id
        )
        AND EXISTS (SELECT 1 FROM implementations WHERE pattern_id = OLD.id)
    )
 )
BEGIN
    SELECT RAISE(ABORT, 'pattern deletion requires archived authored projections');
END;

-- Implementations are authored graph projections, not independently deletable
-- rows. This closes the only route that could otherwise remove a legacy graph
-- first and then make its parent look like a harmless catalog-only pattern.
CREATE TRIGGER require_archived_implementation_before_delete
BEFORE DELETE ON implementations FOR EACH ROW
WHEN (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND NOT EXISTS (
    SELECT 1 FROM authored_state_projections
    WHERE document_kind = 'pattern_graph'
      AND subject_id = OLD.pattern_id
      AND implementation_id = OLD.id
      AND materialization_state = 'archived'
 )
BEGIN
    SELECT RAISE(ABORT, 'implementation deletion requires an archived authored projection');
END;
