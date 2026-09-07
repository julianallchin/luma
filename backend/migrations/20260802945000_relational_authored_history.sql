-- Checkpoint-to-relational authored-state transition.
--
-- Every earlier migration is frozen byte-for-byte because sqlx records and
-- validates its checksum. The Git-backed authored-state experiment was never
-- released, so this boundary preserves its live relational projection and
-- immutable conversation transcript, but deliberately does not import bare
-- repository DAGs or retain their commit/ref ledger. The existing score and
-- graph projections are imported through the real Rust codecs as deterministic
-- root revisions before sync is admitted; this migration only installs the
-- relational substrate and transforms the transcript representation losslessly.

-- Preserve the old linear positions long enough to translate append receipts
-- after message rows become principal-bound parent-linked nodes.
CREATE TABLE relational_upgrade_message_positions (
    thread_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    PRIMARY KEY (thread_id, seq)
);

INSERT INTO relational_upgrade_message_positions (thread_id, seq, message_id)
SELECT thread_id, seq, id
FROM agent_thread_messages;

CREATE TABLE relational_upgrade_appends (
    thread_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    first_seq INTEGER NOT NULL,
    message_count INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (thread_id, operation_id)
);

INSERT INTO relational_upgrade_appends
SELECT thread_id, operation_id, request_fingerprint, first_seq,
       message_count, created_at
FROM agent_thread_message_appends;

-- Terminal checkpoint routes may no longer have a live score/graph projection
-- to feed through the codecs, but dropping their identity would permit stale
-- catalog sync to resurrect a user deletion. Keep a concrete one-time queue;
-- admitted Rust bootstrap derives the new canonical document IDs, emits
-- ordinary headless archive receipts, then drains it. No Git commit/tree is
-- imported and no content revision is invented.
CREATE TABLE relational_upgrade_archived_routes (
    legacy_repository_id TEXT PRIMARY KEY,
    document_kind        TEXT NOT NULL
        CHECK (document_kind IN ('track_score', 'pattern_graph')),
    principal_key        TEXT NOT NULL,
    subject_id           TEXT NOT NULL,
    track_id             TEXT,
    venue_id             TEXT,
    score_id             TEXT,
    implementation_id    TEXT,
    created_at           TEXT NOT NULL,
    archived_at          TEXT NOT NULL,
    CHECK (
        (
            document_kind = 'track_score'
            AND track_id IS NOT NULL
            AND venue_id IS NOT NULL
            AND score_id IS NOT NULL
            AND implementation_id IS NULL
        )
        OR
        (
            document_kind = 'pattern_graph'
            AND track_id IS NULL
            AND venue_id IS NULL
            AND score_id IS NULL
            AND implementation_id IS NOT NULL
        )
    )
);

INSERT INTO relational_upgrade_archived_routes (
    legacy_repository_id, document_kind, principal_key, subject_id, track_id,
    venue_id, score_id, implementation_id, created_at, archived_at
)
SELECT repository_id, document_kind, principal_key, subject_id, track_id,
       venue_id, score_id, implementation_id, created_at, updated_at
FROM authored_state_projections
WHERE materialization_state = 'archived';

-- Triggers attached to surviving transcript/catalog tables still reference
-- the Git-era turn/projection tables. Drop those cross-table dependencies
-- before removing the abandoned authority; relational equivalents are
-- installed below after their replacement tables exist.
DROP TRIGGER IF EXISTS assistant_message_requires_prepared_authored_turn;
DROP TRIGGER IF EXISTS authored_turn_message_id_cannot_be_reused;
DROP TRIGGER IF EXISTS prevent_archived_pattern_resurrection;
DROP TRIGGER IF EXISTS prevent_archived_score_resurrection;
DROP TRIGGER IF EXISTS require_archived_implementation_before_delete;
DROP TRIGGER IF EXISTS require_archived_pattern_before_delete;
DROP TRIGGER IF EXISTS require_archived_score_before_delete;

-- Old deletion receipts named a Git repository after their thread and route
-- had already gone away. They cannot be rebound to a relational document
-- without inventing identity, so this unreleased checkpoint-only receipt is
-- retired. Live threads and every transcript message are preserved below.
DROP TABLE agent_thread_message_appends;
DROP TABLE agent_thread_deletions;

-- Remove the abandoned Git authority from leaves to roots. Domain projection
-- tables (`track_scores`, `implementations`, scores, and patterns) remain and
-- are the canonical bytes consumed by the one-time codec bootstrap.
DROP TABLE authored_state_worktrees;
DROP TABLE authored_state_operations;
DROP TABLE authored_state_turn_commits;
DROP TABLE authored_state_thread_branches;
DROP TABLE authored_state_creations;
DROP TABLE authored_state_projections;

-- These objects already exist in the frozen checkpoint schema. The relational
-- definitions below recreate them after adding conversation forks and the
-- immutable transcript DAG.
DROP INDEX idx_agent_threads_owner_lifecycle_updated;
DROP TRIGGER agent_thread_deletion_is_terminal;

-- Immutable relational authored-document history.
--
-- Canonical score/graph bytes and their complete revision DAG live in the app
-- database. `authored_document_heads` is the only current-state pointer. A
-- writer inserts a revision, updates the live domain projection, and advances
-- the head with compare-and-swap in one SQLite transaction.

CREATE TABLE authored_documents (
    document_id          TEXT PRIMARY KEY,
    document_kind        TEXT NOT NULL
        CHECK (document_kind IN ('track_score', 'pattern_graph')),
    principal_key        TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    subject_id           TEXT NOT NULL,
    track_id             TEXT,
    venue_id             TEXT,
    score_id             TEXT,
    implementation_id    TEXT,
    archived_at          TEXT,
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (principal_key, document_id),
    CHECK (
        (
            document_kind = 'track_score'
            AND track_id IS NOT NULL
            AND venue_id IS NOT NULL
            AND score_id IS NOT NULL
            AND implementation_id IS NULL
        )
        OR
        (
            document_kind = 'pattern_graph'
            AND track_id IS NULL
            AND venue_id IS NULL
            AND score_id IS NULL
            AND implementation_id IS NOT NULL
        )
    )
);

CREATE UNIQUE INDEX idx_authored_documents_track_scope
    ON authored_documents(principal_key, track_id, venue_id, score_id)
    WHERE document_kind = 'track_score';

CREATE UNIQUE INDEX idx_authored_documents_pattern_scope
    ON authored_documents(principal_key, subject_id, implementation_id)
    WHERE document_kind = 'pattern_graph';

CREATE TRIGGER authored_document_identity_is_immutable
BEFORE UPDATE ON authored_documents FOR EACH ROW
WHEN NEW.document_id IS NOT OLD.document_id
  OR NEW.document_kind IS NOT OLD.document_kind
  OR NEW.principal_key IS NOT OLD.principal_key
  OR NEW.subject_id IS NOT OLD.subject_id
  OR NEW.track_id IS NOT OLD.track_id
  OR NEW.venue_id IS NOT OLD.venue_id
  OR NEW.score_id IS NOT OLD.score_id
  OR NEW.implementation_id IS NOT OLD.implementation_id
  OR NEW.created_at IS NOT OLD.created_at
BEGIN
    SELECT RAISE(ABORT, 'authored document identity is immutable');
END;

CREATE TRIGGER authored_document_archive_is_terminal
BEFORE UPDATE OF archived_at ON authored_documents FOR EACH ROW
WHEN OLD.archived_at IS NOT NULL AND NEW.archived_at IS NOT OLD.archived_at
BEGIN
    SELECT RAISE(ABORT, 'archived authored document cannot be restored');
END;

CREATE TRIGGER authored_document_history_is_permanent
BEFORE DELETE ON authored_documents FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored document history is permanent');
END;

CREATE TABLE authored_revisions (
    revision_id          TEXT PRIMARY KEY,
    document_id          TEXT NOT NULL,
    principal_key        TEXT NOT NULL,
    parent_count         INTEGER NOT NULL CHECK (parent_count IN (0, 1, 2)),
    content_hash         TEXT NOT NULL,
    operation_kind       TEXT NOT NULL,
    operation_id         TEXT,
    message              TEXT NOT NULL,
    author_name          TEXT NOT NULL,
    author_email         TEXT NOT NULL,
    authored_at          TEXT NOT NULL,
    thread_id            TEXT,
    assistant_message_id TEXT,
    restored_revision_id TEXT,
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (principal_key, document_id)
        REFERENCES authored_documents(principal_key, document_id),
    FOREIGN KEY (principal_key, document_id, restored_revision_id)
        REFERENCES authored_revisions(principal_key, document_id, revision_id),
    CHECK (assistant_message_id IS NULL OR thread_id IS NOT NULL),
    UNIQUE (document_id, revision_id),
    UNIQUE (principal_key, revision_id),
    UNIQUE (principal_key, document_id, revision_id)
);

CREATE INDEX idx_authored_revisions_document_time
    ON authored_revisions(principal_key, document_id, authored_at DESC, revision_id DESC);

CREATE UNIQUE INDEX idx_authored_revisions_operation
    ON authored_revisions(document_id, operation_kind, operation_id)
    WHERE operation_id IS NOT NULL;

CREATE UNIQUE INDEX idx_authored_revisions_assistant_message
    ON authored_revisions(assistant_message_id)
    WHERE assistant_message_id IS NOT NULL;

CREATE TRIGGER authored_revision_is_immutable
BEFORE UPDATE ON authored_revisions FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored revision is immutable');
END;

CREATE TRIGGER authored_revision_is_permanent
BEFORE DELETE ON authored_revisions FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored revision is permanent');
END;

CREATE TRIGGER authored_revision_restore_stays_in_document
BEFORE INSERT ON authored_revisions FOR EACH ROW
WHEN NEW.restored_revision_id IS NOT NULL
 AND NOT EXISTS (
     SELECT 1 FROM authored_revisions restored
     WHERE restored.revision_id = NEW.restored_revision_id
       AND restored.document_id = NEW.document_id
       AND restored.principal_key = NEW.principal_key
 )
BEGIN
    SELECT RAISE(ABORT, 'restored revision must belong to the same document');
END;

-- Exact per-path canonical bytes. The revision content hash is a manifest hash
-- over ordered `(path, bytes)` pairs; the per-file hash makes collisions and
-- sync corruption diagnosable without decoding a domain document.
CREATE TABLE authored_revision_files (
    revision_id          TEXT NOT NULL,
    principal_key        TEXT NOT NULL,
    path                 TEXT NOT NULL,
    content_hash         TEXT NOT NULL,
    content              BLOB NOT NULL,
    PRIMARY KEY (revision_id, path),
    FOREIGN KEY (principal_key, revision_id)
        REFERENCES authored_revisions(principal_key, revision_id)
);

CREATE TRIGGER authored_revision_file_is_immutable
BEFORE UPDATE ON authored_revision_files FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored revision file is immutable');
END;

CREATE TRIGGER authored_revision_file_is_permanent
BEFORE DELETE ON authored_revision_files FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored revision file is permanent');
END;

-- Parent order is semantic: parent 0 is the mainline/ours parent and parent 1
-- is the merged/theirs parent. A revision has zero, one, or two parents.
CREATE TABLE authored_revision_parents (
    principal_key        TEXT NOT NULL,
    document_id          TEXT NOT NULL,
    revision_id          TEXT NOT NULL,
    parent_order         INTEGER NOT NULL CHECK (parent_order IN (0, 1)),
    parent_revision_id   TEXT NOT NULL,
    CHECK (revision_id <> parent_revision_id),
    PRIMARY KEY (revision_id, parent_order),
    UNIQUE (revision_id, parent_revision_id),
    FOREIGN KEY (principal_key, document_id, revision_id)
        REFERENCES authored_revisions(principal_key, document_id, revision_id),
    FOREIGN KEY (principal_key, document_id, parent_revision_id)
        REFERENCES authored_revisions(principal_key, document_id, revision_id)
);

CREATE INDEX idx_authored_revision_parents_parent
    ON authored_revision_parents(principal_key, document_id, parent_revision_id);

CREATE TRIGGER authored_revision_parent_is_immutable
BEFORE UPDATE ON authored_revision_parents FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored revision parent is immutable');
END;

CREATE TRIGGER authored_revision_parent_matches_declared_shape
BEFORE INSERT ON authored_revision_parents FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM authored_revisions revision
    WHERE revision.revision_id = NEW.revision_id
      AND revision.document_id = NEW.document_id
      AND revision.principal_key = NEW.principal_key
      AND NEW.parent_order < revision.parent_count
)
BEGIN
    SELECT RAISE(ABORT, 'authored parent edge exceeds immutable revision shape');
END;

CREATE TRIGGER authored_revision_parent_is_permanent
BEFORE DELETE ON authored_revision_parents FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored revision parent is permanent');
END;

CREATE TABLE authored_document_heads (
    document_id          TEXT PRIMARY KEY,
    principal_key        TEXT NOT NULL,
    revision_id          TEXT NOT NULL,
    generation           INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0),
    updated_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (principal_key, document_id)
        REFERENCES authored_documents(principal_key, document_id),
    FOREIGN KEY (principal_key, document_id, revision_id)
        REFERENCES authored_revisions(principal_key, document_id, revision_id)
);

CREATE TRIGGER authored_document_head_requires_active_document
BEFORE INSERT ON authored_document_heads FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_id = NEW.document_id
      AND document.archived_at IS NOT NULL
)
BEGIN
    SELECT RAISE(ABORT, 'archived authored document cannot acquire a head');
END;

CREATE TRIGGER authored_document_head_advance_requires_active_document
BEFORE UPDATE OF revision_id, generation ON authored_document_heads FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_id = NEW.document_id
      AND document.archived_at IS NOT NULL
)
BEGIN
    SELECT RAISE(ABORT, 'archived authored document head cannot advance');
END;

CREATE TRIGGER authored_document_head_is_strict_cas_counter
BEFORE UPDATE OF revision_id, generation ON authored_document_heads FOR EACH ROW
WHEN NEW.revision_id IS OLD.revision_id
  OR NEW.generation <> OLD.generation + 1
BEGIN
    SELECT RAISE(ABORT, 'authored document head advance must increment generation once');
END;

CREATE TRIGGER authored_document_head_updated_at
AFTER UPDATE OF revision_id, generation ON authored_document_heads FOR EACH ROW
BEGIN
    UPDATE authored_document_heads
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE document_id = OLD.document_id;
END;

CREATE TRIGGER authored_document_head_identity_is_immutable
BEFORE UPDATE OF document_id, principal_key ON authored_document_heads FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored document head identity is immutable');
END;

CREATE TRIGGER authored_document_head_is_permanent
BEFORE DELETE ON authored_document_heads FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored document head is permanent');
END;
-- A fork is a new conversation identity whose transcript head initially
-- points at an immutable message in another same-principal thread. The source
-- thread and cut point are audit metadata, not ownership: deleting the source
-- must never rewrite or delete a shared prefix.
ALTER TABLE agent_threads ADD COLUMN forked_from_thread_id TEXT;
ALTER TABLE agent_threads ADD COLUMN forked_at_message_id TEXT;

CREATE INDEX idx_agent_threads_owner_lifecycle_updated
    ON agent_threads(owner_user_id, lifecycle_state, updated_at DESC);

CREATE TRIGGER agent_thread_deletion_is_terminal
BEFORE UPDATE OF lifecycle_state ON agent_threads
FOR EACH ROW
WHEN OLD.lifecycle_state = 'deleting' AND NEW.lifecycle_state != 'deleting'
BEGIN
    SELECT RAISE(ABORT, 'deleting agent thread cannot be reactivated');
END;

-- Convert the old thread-owned `(thread_id, seq)` rows into a principal-bound
-- immutable parent chain. Message IDs and payload bytes are copied verbatim.
-- `created_in_thread_id` is provenance only and deliberately has no FK: a
-- shared transcript node outlives the thread in which it was first appended.
ALTER TABLE agent_thread_messages RENAME TO agent_thread_messages_linear;

CREATE TABLE agent_thread_messages (
    id                   TEXT PRIMARY KEY,
    owner_user_id        TEXT,
    principal_key        TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    created_in_thread_id TEXT NOT NULL,
    parent_message_id    TEXT,
    depth                INTEGER NOT NULL CHECK (depth >= 0),
    role                 TEXT NOT NULL,
    parts_json           TEXT NOT NULL
        CHECK (json_valid(parts_json) AND json_type(parts_json) = 'array'),
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    FOREIGN KEY (parent_message_id) REFERENCES agent_thread_messages(id),
    CHECK (
        (parent_message_id IS NULL AND depth = 0)
        OR (parent_message_id IS NOT NULL AND depth > 0)
    )
);

INSERT INTO agent_thread_messages (
    id, owner_user_id, principal_key, created_in_thread_id, parent_message_id,
    depth, role, parts_json, created_at
)
SELECT
    ordered.id,
    thread.owner_user_id,
    CASE
        WHEN thread.owner_user_id IS NULL THEN 'signed-out'
        ELSE 'signed-in:' || thread.owner_user_id
    END,
    ordered.thread_id,
    ordered.parent_message_id,
    ordered.depth,
    ordered.role,
    ordered.parts_json,
    ordered.created_at
FROM (
    SELECT
        message.*,
        lag(message.id) OVER (
            PARTITION BY message.thread_id
            ORDER BY message.seq, message.id
        ) AS parent_message_id,
        row_number() OVER (
            PARTITION BY message.thread_id
            ORDER BY message.seq, message.id
        ) - 1 AS depth
    FROM agent_thread_messages_linear AS message
) AS ordered
JOIN agent_threads AS thread ON thread.id = ordered.thread_id
ORDER BY ordered.thread_id, ordered.depth;

DROP TABLE agent_thread_messages_linear;

CREATE INDEX idx_agent_thread_messages_parent
    ON agent_thread_messages(parent_message_id);
CREATE INDEX idx_agent_thread_messages_owner_created
    ON agent_thread_messages(owner_user_id, created_at, id);
CREATE INDEX idx_agent_thread_messages_principal_created
    ON agent_thread_messages(principal_key, created_at, id);
CREATE INDEX idx_agent_thread_messages_created_in_thread
    ON agent_thread_messages(created_in_thread_id, created_at, id);

-- One mutable compare-and-swap pointer per conversation. All transcript bytes
-- live in the immutable node table above; a fork shares a prefix by copying
-- only this pointer and count.
CREATE TABLE agent_thread_transcript_heads (
    thread_id        TEXT PRIMARY KEY,
    owner_user_id    TEXT,
    head_message_id  TEXT,
    message_count    INTEGER NOT NULL DEFAULT 0 CHECK (message_count >= 0),
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    FOREIGN KEY (thread_id) REFERENCES agent_threads(id) ON DELETE CASCADE,
    FOREIGN KEY (head_message_id) REFERENCES agent_thread_messages(id),
    CHECK (
        (head_message_id IS NULL AND message_count = 0)
        OR (head_message_id IS NOT NULL AND message_count > 0)
    )
);

INSERT INTO agent_thread_transcript_heads (
    thread_id, owner_user_id, head_message_id, message_count, updated_at
)
SELECT
    thread.id,
    thread.owner_user_id,
    (
        SELECT message.id
        FROM agent_thread_messages AS message
        WHERE message.created_in_thread_id = thread.id
        ORDER BY message.depth DESC, message.id DESC
        LIMIT 1
    ),
    (
        SELECT COUNT(*)
        FROM agent_thread_messages AS message
        WHERE message.created_in_thread_id = thread.id
    ),
    thread.updated_at
FROM agent_threads AS thread;

CREATE INDEX idx_agent_thread_transcript_heads_owner
    ON agent_thread_transcript_heads(owner_user_id, updated_at DESC);

CREATE TRIGGER agent_thread_create_empty_transcript
AFTER INSERT ON agent_threads FOR EACH ROW
BEGIN
    INSERT INTO agent_thread_transcript_heads (thread_id, owner_user_id)
    VALUES (NEW.id, NEW.owner_user_id);
END;

CREATE TRIGGER agent_thread_transcript_head_identity_is_immutable
BEFORE UPDATE ON agent_thread_transcript_heads FOR EACH ROW
WHEN NEW.thread_id IS NOT OLD.thread_id
  OR NEW.owner_user_id IS NOT OLD.owner_user_id
BEGIN
    SELECT RAISE(ABORT, 'agent transcript head identity is immutable');
END;

CREATE TRIGGER agent_thread_transcript_head_matches_owner_insert
BEFORE INSERT ON agent_thread_transcript_heads FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads AS thread
    WHERE thread.id = NEW.thread_id
      AND thread.owner_user_id IS NEW.owner_user_id
)
OR (
    NEW.head_message_id IS NOT NULL
    AND NOT EXISTS (
        SELECT 1 FROM agent_thread_messages AS message
        WHERE message.id = NEW.head_message_id
          AND message.owner_user_id IS NEW.owner_user_id
          AND message.principal_key = CASE
                WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
                ELSE 'signed-in:' || NEW.owner_user_id
              END
          AND message.depth + 1 = NEW.message_count
    )
)
BEGIN
    SELECT RAISE(ABORT, 'agent transcript head does not match its principal or length');
END;

CREATE TRIGGER agent_thread_transcript_head_matches_owner_update
BEFORE UPDATE OF head_message_id, message_count ON agent_thread_transcript_heads FOR EACH ROW
WHEN (
    NEW.head_message_id IS NULL AND NEW.message_count != 0
)
OR (
    NEW.head_message_id IS NOT NULL
    AND NOT EXISTS (
        SELECT 1 FROM agent_thread_messages AS message
        WHERE message.id = NEW.head_message_id
          AND message.owner_user_id IS NEW.owner_user_id
          AND message.principal_key = CASE
                WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
                ELSE 'signed-in:' || NEW.owner_user_id
              END
          AND message.depth + 1 = NEW.message_count
    )
)
BEGIN
    SELECT RAISE(ABORT, 'agent transcript head does not match its principal or length');
END;
-- A turn preparation reserves the exact immutable authored revision before
-- the assistant transcript node is persisted. The outcome is a second
-- immutable row written only after that node exists; absence of an outcome is
-- therefore the complete crash-recovery query.
CREATE TABLE authored_turn_preparations (
    thread_id             TEXT NOT NULL,
    assistant_message_id  TEXT NOT NULL,
    owner_user_id         TEXT,
    principal_key         TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    document_id           TEXT NOT NULL,
    prepared_revision_id  TEXT NOT NULL,
    created_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    PRIMARY KEY (thread_id, assistant_message_id),
    UNIQUE (assistant_message_id),
    FOREIGN KEY (principal_key, document_id, prepared_revision_id)
        REFERENCES authored_revisions(principal_key, document_id, revision_id)
);

CREATE TABLE authored_turn_outcomes (
    thread_id             TEXT NOT NULL,
    assistant_message_id  TEXT NOT NULL,
    owner_user_id         TEXT,
    principal_key         TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    document_id           TEXT NOT NULL,
    prepared_revision_id  TEXT NOT NULL,
    status                TEXT NOT NULL CHECK (status IN ('committed', 'conflicted')),
    result_revision_id    TEXT,
    conflicts_json        TEXT
        CHECK (conflicts_json IS NULL OR (json_valid(conflicts_json) AND json_type(conflicts_json) = 'array')),
    created_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    PRIMARY KEY (thread_id, assistant_message_id),
    FOREIGN KEY (thread_id, assistant_message_id)
        REFERENCES authored_turn_preparations(thread_id, assistant_message_id),
    FOREIGN KEY (principal_key, document_id, prepared_revision_id)
        REFERENCES authored_revisions(principal_key, document_id, revision_id),
    FOREIGN KEY (principal_key, document_id, result_revision_id)
        REFERENCES authored_revisions(principal_key, document_id, revision_id),
    CHECK (
        (status = 'committed' AND result_revision_id IS NOT NULL AND conflicts_json IS NULL)
        OR
        (status = 'conflicted' AND result_revision_id IS NULL AND conflicts_json IS NOT NULL)
    )
);

CREATE TRIGGER authored_turn_preparation_requires_active_thread
BEFORE INSERT ON authored_turn_preparations FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1
    FROM agent_threads AS thread
    JOIN authored_documents AS document
      ON document.document_id = NEW.document_id
     AND document.principal_key = NEW.principal_key
    WHERE thread.id = NEW.thread_id
      AND thread.owner_user_id IS NEW.owner_user_id
      AND NEW.principal_key = CASE
            WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || NEW.owner_user_id
      END
      AND thread.lifecycle_state = 'active'
      AND document.archived_at IS NULL
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
BEGIN
    SELECT RAISE(ABORT, 'authored turn preparation requires an active thread');
END;

CREATE TRIGGER authored_turn_outcome_requires_active_thread
BEFORE INSERT ON authored_turn_outcomes FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads AS thread
    JOIN authored_turn_preparations AS preparation
      ON preparation.thread_id = NEW.thread_id
     AND preparation.assistant_message_id = NEW.assistant_message_id
    WHERE thread.id = NEW.thread_id
      AND thread.owner_user_id IS NEW.owner_user_id
      AND preparation.owner_user_id IS NEW.owner_user_id
      AND NEW.principal_key = CASE
            WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || NEW.owner_user_id
          END
      AND preparation.principal_key = NEW.principal_key
      AND preparation.document_id = NEW.document_id
      AND preparation.prepared_revision_id = NEW.prepared_revision_id
      AND thread.lifecycle_state = 'active'
      AND EXISTS (
          SELECT 1
          FROM agent_thread_messages AS message
          WHERE message.id = NEW.assistant_message_id
            AND message.created_in_thread_id = NEW.thread_id
            AND message.owner_user_id IS NEW.owner_user_id
            AND message.principal_key = NEW.principal_key
            AND message.role = 'assistant'
      )
)
BEGIN
    SELECT RAISE(ABORT, 'authored turn outcome does not match its active preparation');
END;

CREATE TRIGGER authored_turn_preparation_is_immutable
BEFORE UPDATE ON authored_turn_preparations FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored turn preparation is immutable'); END;

CREATE TRIGGER authored_turn_preparation_is_permanent
BEFORE DELETE ON authored_turn_preparations FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored turn preparation is permanent'); END;

CREATE TRIGGER authored_turn_outcome_is_immutable
BEFORE UPDATE ON authored_turn_outcomes FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored turn outcome is immutable'); END;

CREATE TRIGGER authored_turn_outcome_is_permanent
BEFORE DELETE ON authored_turn_outcomes FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored turn outcome is permanent'); END;

-- One durable replay receipt per atomic transcript append. The receipt points
-- at the immutable message range rather than duplicating message/tool payloads;
-- exact generated IDs and timestamps replay from the message rows themselves.
-- These rows are transcript trace machinery and survive thread deletion.
CREATE TABLE agent_thread_message_appends (
    thread_id           TEXT NOT NULL,
    owner_user_id       TEXT,
    principal_key       TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    operation_id        TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    base_head_message_id TEXT,
    first_message_id    TEXT NOT NULL,
    result_head_message_id TEXT NOT NULL,
    message_count       INTEGER NOT NULL CHECK (message_count > 0),
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    PRIMARY KEY (thread_id, operation_id),
    FOREIGN KEY (base_head_message_id) REFERENCES agent_thread_messages(id),
    FOREIGN KEY (first_message_id) REFERENCES agent_thread_messages(id),
    FOREIGN KEY (result_head_message_id) REFERENCES agent_thread_messages(id)
);

CREATE TRIGGER agent_thread_append_requires_active_thread
BEFORE INSERT ON agent_thread_message_appends FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads
    WHERE id = NEW.thread_id
      AND owner_user_id IS NEW.owner_user_id
      AND NEW.principal_key = CASE
            WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || NEW.owner_user_id
          END
      AND lifecycle_state = 'active'
)
OR (
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
        UNION ALL
        SELECT parent.id, parent.parent_message_id
        FROM agent_thread_messages AS parent
        JOIN lineage AS child ON child.parent_message_id = parent.id
    )
    SELECT 1
    FROM lineage
    JOIN agent_thread_messages AS first ON first.id = NEW.first_message_id
    JOIN agent_thread_messages AS result ON result.id = NEW.result_head_message_id
    WHERE lineage.id = first.id
      AND first.owner_user_id IS NEW.owner_user_id
      AND first.principal_key = NEW.principal_key
      AND first.parent_message_id IS NEW.base_head_message_id
      AND result.depth - first.depth + 1 = NEW.message_count
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread append receipt has invalid scope or range');
END;

-- Reservation precedes assistant persistence. A message that already exists
-- cannot be retroactively claimed by a new turn; an exact same-row retry is
-- allowed so preparation remains recoverable after response loss.
CREATE TRIGGER authored_turn_message_reservation_requires_unused_id
BEFORE INSERT ON authored_turn_preparations FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM agent_thread_messages
    WHERE id = NEW.assistant_message_id
)
AND NOT EXISTS (
    SELECT 1 FROM authored_turn_preparations
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
    FROM authored_turn_preparations
    WHERE assistant_message_id = NEW.id
)
AND NOT (
    NEW.role = 'assistant'
    AND EXISTS (
        SELECT 1
        FROM authored_turn_preparations AS authored_turn
        JOIN agent_threads AS thread
          ON thread.id = authored_turn.thread_id
        WHERE authored_turn.thread_id = NEW.created_in_thread_id
          AND authored_turn.assistant_message_id = NEW.id
          AND authored_turn.owner_user_id IS NEW.owner_user_id
          AND authored_turn.principal_key = NEW.principal_key
          AND thread.lifecycle_state = 'active'
    )
)
BEGIN
    SELECT RAISE(ABORT, 'authored turn message identity is immutable');
END;

-- Persisted messages are an append-only conversation DAG. Redo is a new turn,
-- rewind changes authored state rather than the transcript, and forks share
-- existing nodes. Message nodes therefore have no update or delete lifecycle.
CREATE TRIGGER agent_thread_message_cannot_be_updated
BEFORE UPDATE ON agent_thread_messages FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent thread transcript is append-only');
END;

CREATE TRIGGER agent_thread_message_cannot_be_deleted
BEFORE DELETE ON agent_thread_messages FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent thread transcript nodes are immutable');
END;

CREATE TRIGGER agent_thread_message_requires_valid_parent
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads AS thread
    WHERE thread.id = NEW.created_in_thread_id
      AND thread.owner_user_id IS NEW.owner_user_id
      AND NEW.principal_key = CASE
            WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || NEW.owner_user_id
          END
      AND thread.lifecycle_state = 'active'
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
    SELECT RAISE(ABORT, 'agent transcript message has an invalid parent or principal');
END;

-- A replay receipt is the permanent identity of the exact committed batch.
CREATE TRIGGER agent_thread_append_receipt_is_immutable
BEFORE UPDATE ON agent_thread_message_appends FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent thread append receipt is immutable');
END;

CREATE TRIGGER active_agent_thread_append_receipt_cannot_be_deleted
BEFORE DELETE ON agent_thread_message_appends FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent thread append receipt is permanent');
END;

-- Translate every old `(first_seq, message_count)` receipt to the exact
-- immutable node range. Do this before admission triggers are installed so an
-- interrupted terminal deletion retains its trace as well.
DROP TRIGGER agent_thread_append_requires_active_thread;

INSERT INTO agent_thread_message_appends (
    thread_id, owner_user_id, principal_key, operation_id,
    request_fingerprint, base_head_message_id, first_message_id,
    result_head_message_id, message_count, created_at
)
SELECT
    receipt.thread_id,
    thread.owner_user_id,
    CASE
        WHEN thread.owner_user_id IS NULL THEN 'signed-out'
        ELSE 'signed-in:' || thread.owner_user_id
    END,
    receipt.operation_id,
    receipt.request_fingerprint,
    base.message_id,
    first.message_id,
    result.message_id,
    receipt.message_count,
    receipt.created_at
FROM relational_upgrade_appends AS receipt
JOIN agent_threads AS thread
  ON thread.id = receipt.thread_id
JOIN relational_upgrade_message_positions AS first
  ON first.thread_id = receipt.thread_id
 AND first.seq = receipt.first_seq
JOIN relational_upgrade_message_positions AS result
  ON result.thread_id = receipt.thread_id
 AND result.seq = receipt.first_seq + receipt.message_count - 1
LEFT JOIN relational_upgrade_message_positions AS base
  ON base.thread_id = receipt.thread_id
 AND base.seq = receipt.first_seq - 1;

CREATE TRIGGER agent_thread_append_requires_active_thread
BEFORE INSERT ON agent_thread_message_appends FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads
    WHERE id = NEW.thread_id
      AND owner_user_id IS NEW.owner_user_id
      AND NEW.principal_key = CASE
            WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || NEW.owner_user_id
          END
      AND lifecycle_state = 'active'
)
OR (
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
        UNION ALL
        SELECT parent.id, parent.parent_message_id
        FROM agent_thread_messages AS parent
        JOIN lineage AS child ON child.parent_message_id = parent.id
    )
    SELECT 1
    FROM lineage
    JOIN agent_thread_messages AS first ON first.id = NEW.first_message_id
    JOIN agent_thread_messages AS result ON result.id = NEW.result_head_message_id
    WHERE lineage.id = first.id
      AND first.owner_user_id IS NEW.owner_user_id
      AND first.principal_key = NEW.principal_key
      AND first.parent_message_id IS NEW.base_head_message_id
      AND result.depth - first.depth + 1 = NEW.message_count
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread append receipt has invalid scope or range');
END;

DROP TABLE relational_upgrade_appends;
DROP TABLE relational_upgrade_message_positions;
-- Canonical authored payloads now synchronize through immutable document
-- revisions. `track_scores`, `implementations.graph_json`, and per-venue
-- implementation routing are live projections and must not travel through
-- generic row sync as a second authored-state authority.

DROP TRIGGER IF EXISTS sync_delete_implementations;
DROP TRIGGER IF EXISTS sync_delete_track_scores;
DROP TRIGGER IF EXISTS sync_delete_venue_impl_overrides;

-- Discard payloads queued by older branch builds. The relational revision
-- transaction enqueues the canonical replacement; replaying these opaque live
-- projections would create a second authored-state authority.
DELETE FROM pending_ops
WHERE table_name IN (
    'implementations', 'track_scores', 'venue_implementation_overrides'
);

DELETE FROM sync_state
WHERE table_name IN (
    'implementations', 'track_scores', 'venue_implementation_overrides'
);
-- Durable authored-operation outcomes, isolated subagent workspaces, and the
-- local half of server-ordered head convergence. Revision bytes and lineage
-- live in the core authored tables created by 202608011.

-- One installation identity survives sign-in changes and gives the server a
-- stable device component for proposal ordering and idempotency. It is local
-- infrastructure, not user data, and is therefore never synchronized.
CREATE TABLE authored_device_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    device_id  TEXT NOT NULL UNIQUE,
    CHECK (
        length(device_id) = 36
        AND substr(device_id, 9, 1) = '-'
        AND substr(device_id, 14, 1) = '-'
        AND substr(device_id, 19, 1) = '-'
        AND substr(device_id, 24, 1) = '-'
    )
);

INSERT INTO authored_device_identity (singleton, device_id)
VALUES (
    1,
    lower(
        hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
        substr(hex(randomblob(2)), 2) || '-' ||
        substr('89ab', 1 + (abs(random()) % 4), 1) ||
        substr(hex(randomblob(2)), 2) || '-' || hex(randomblob(6))
    )
);

CREATE TRIGGER authored_device_identity_is_immutable
BEFORE UPDATE ON authored_device_identity FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored device identity is immutable'); END;

CREATE TRIGGER authored_device_identity_is_permanent
BEFORE DELETE ON authored_device_identity FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored device identity is permanent'); END;

CREATE TABLE authored_operation_outcomes (
    principal_key        TEXT NOT NULL,
    document_id          TEXT NOT NULL,
    operation_kind       TEXT NOT NULL CHECK (
        operation_kind IN (
            'create_score', 'create_pattern', 'score_edit', 'graph_edit',
            'restore', 'workspace_commit', 'workspace_merge', 'pattern_fork'
        )
    ),
    operation_id         TEXT NOT NULL,
    request_fingerprint  TEXT NOT NULL,
    base_revision_id     TEXT,
    status               TEXT NOT NULL CHECK (status IN ('committed', 'conflicted')),
    result_revision_id   TEXT,
    conflicts_json       TEXT,
    result_json          TEXT,
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (document_id, operation_kind, operation_id),
    FOREIGN KEY (document_id) REFERENCES authored_documents(document_id),
    FOREIGN KEY (document_id, base_revision_id)
        REFERENCES authored_revisions(document_id, revision_id),
    FOREIGN KEY (document_id, result_revision_id)
        REFERENCES authored_revisions(document_id, revision_id),
    CHECK (
        (status = 'committed' AND result_revision_id IS NOT NULL AND conflicts_json IS NULL)
        OR
        (status = 'conflicted' AND result_revision_id IS NULL AND conflicts_json IS NOT NULL)
    )
);

CREATE TRIGGER authored_operation_principal_matches_document
BEFORE INSERT ON authored_operation_outcomes FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_id = NEW.document_id
      AND document.principal_key = NEW.principal_key
)
BEGIN SELECT RAISE(ABORT, 'authored operation principal does not own document'); END;

CREATE TRIGGER authored_operation_outcome_is_immutable
BEFORE UPDATE ON authored_operation_outcomes FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored operation outcome is immutable'); END;

CREATE TRIGGER authored_operation_outcome_is_permanent
BEFORE DELETE ON authored_operation_outcomes FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored operation outcome is permanent'); END;

-- A workspace is a disposable plain directory whose durable identity is its
-- recorded base/head revision. It is never a branch, ref, or source of truth.
CREATE TABLE authored_subagent_workspaces (
    workspace_id         TEXT PRIMARY KEY,
    request_id           TEXT NOT NULL,
    request_fingerprint  TEXT NOT NULL,
    document_id          TEXT NOT NULL,
    owner_thread_id      TEXT NOT NULL,
    base_revision_id     TEXT NOT NULL,
    head_revision_id     TEXT NOT NULL,
    generation           INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0),
    status               TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'retired')),
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    retired_at           TEXT,
    FOREIGN KEY (document_id) REFERENCES authored_documents(document_id),
    FOREIGN KEY (document_id, base_revision_id)
        REFERENCES authored_revisions(document_id, revision_id),
    FOREIGN KEY (document_id, head_revision_id)
        REFERENCES authored_revisions(document_id, revision_id),
    FOREIGN KEY (owner_thread_id) REFERENCES agent_threads(id) ON DELETE CASCADE,
    UNIQUE (owner_thread_id, request_id),
    CHECK (
        (status = 'active' AND retired_at IS NULL)
        OR (status = 'retired' AND retired_at IS NOT NULL)
    )
);

CREATE TRIGGER authored_workspace_scope_matches_thread
BEFORE INSERT ON authored_subagent_workspaces FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1
    FROM authored_documents document
    JOIN agent_threads thread ON thread.id = NEW.owner_thread_id
    WHERE document.document_id = NEW.document_id
      AND document.principal_key = CASE
          WHEN thread.owner_user_id IS NULL THEN 'signed-out'
          ELSE 'signed-in:' || thread.owner_user_id
      END
)
BEGIN SELECT RAISE(ABORT, 'authored workspace thread does not own document'); END;

CREATE INDEX idx_authored_subagent_workspaces_owner
    ON authored_subagent_workspaces(owner_thread_id, status);

CREATE TRIGGER authored_subagent_workspace_identity_is_immutable
BEFORE UPDATE ON authored_subagent_workspaces FOR EACH ROW
WHEN NEW.workspace_id IS NOT OLD.workspace_id
  OR NEW.request_id IS NOT OLD.request_id
  OR NEW.request_fingerprint IS NOT OLD.request_fingerprint
  OR NEW.document_id IS NOT OLD.document_id
  OR NEW.owner_thread_id IS NOT OLD.owner_thread_id
  OR NEW.base_revision_id IS NOT OLD.base_revision_id
  OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'authored workspace identity is immutable'); END;

CREATE TRIGGER authored_subagent_workspace_transition_is_strict
BEFORE UPDATE ON authored_subagent_workspaces FOR EACH ROW
WHEN NOT (
    (
        OLD.status = 'active' AND NEW.status = 'active'
        AND NEW.head_revision_id IS NOT OLD.head_revision_id
        AND NEW.generation = OLD.generation + 1
        AND NEW.retired_at IS NULL
    )
    OR
    (
        OLD.status = 'active' AND NEW.status = 'retired'
        AND NEW.head_revision_id IS OLD.head_revision_id
        AND NEW.generation = OLD.generation
        AND NEW.retired_at IS NOT NULL
    )
)
BEGIN
    SELECT RAISE(ABORT, 'authored workspace update must advance its head once or retire it');
END;

-- Every local live-head advance becomes one immutable proposal. The server
-- assigns proposal_seq; input identity is never coalesced or overwritten.
CREATE TABLE authored_head_proposals (
    proposal_id          TEXT PRIMARY KEY,
    principal_key        TEXT NOT NULL,
    document_id          TEXT NOT NULL,
    device_id            TEXT NOT NULL,
    operation_id         TEXT NOT NULL,
    base_revision_id     TEXT,
    proposed_revision_id TEXT NOT NULL,
    server_proposal_seq  INTEGER,
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    FOREIGN KEY (document_id) REFERENCES authored_documents(document_id),
    FOREIGN KEY (document_id, base_revision_id)
        REFERENCES authored_revisions(document_id, revision_id),
    FOREIGN KEY (document_id, proposed_revision_id)
        REFERENCES authored_revisions(document_id, revision_id),
    UNIQUE (principal_key, document_id, device_id, operation_id)
);

CREATE TRIGGER authored_head_proposal_principal_matches_document
BEFORE INSERT ON authored_head_proposals FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_id = NEW.document_id
      AND document.principal_key = NEW.principal_key
      AND document.archived_at IS NULL
)
BEGIN SELECT RAISE(ABORT, 'authored proposal principal does not own active document'); END;

CREATE INDEX idx_authored_head_proposals_pending
    ON authored_head_proposals(principal_key, server_proposal_seq, created_at)
    WHERE server_proposal_seq IS NULL;

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
  OR OLD.server_proposal_seq IS NOT NULL
  OR NEW.server_proposal_seq IS NULL
BEGIN SELECT RAISE(ABORT, 'authored head proposal is immutable'); END;

CREATE TRIGGER authored_head_proposal_is_permanent
BEFORE DELETE ON authored_head_proposals FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored head proposal is permanent'); END;

-- Integration is a separate immutable trace. There is no conflict status:
-- even invalid/unreadable proposals finish as a quarantined no-op.
CREATE TABLE authored_head_integrations (
    proposal_id           TEXT PRIMARY KEY,
    principal_key         TEXT NOT NULL,
    document_id           TEXT NOT NULL,
    prior_revision_id     TEXT,
    result_revision_id    TEXT,
    resolution_kind       TEXT NOT NULL CHECK (
        resolution_kind IN (
            'fast_forward', 'already_ancestor', 'structural',
            'whole_proposal', 'quarantined_noop', 'cancelled_archived'
        )
    ),
    server_integration_seq INTEGER NOT NULL,
    integrated_at         TEXT NOT NULL,
    FOREIGN KEY (proposal_id) REFERENCES authored_head_proposals(proposal_id),
    FOREIGN KEY (document_id, prior_revision_id)
        REFERENCES authored_revisions(document_id, revision_id),
    FOREIGN KEY (document_id, result_revision_id)
        REFERENCES authored_revisions(document_id, revision_id),
    CHECK (
        (resolution_kind = 'cancelled_archived'
            AND prior_revision_id IS NULL AND result_revision_id IS NULL)
        OR
        (resolution_kind = 'quarantined_noop'
            AND prior_revision_id IS result_revision_id)
        OR
        (resolution_kind IN ('fast_forward', 'whole_proposal')
            AND result_revision_id IS NOT NULL)
        OR
        (resolution_kind NOT IN (
                'cancelled_archived', 'quarantined_noop',
                'fast_forward', 'whole_proposal'
            )
            AND prior_revision_id IS NOT NULL AND result_revision_id IS NOT NULL)
    )
);

CREATE TRIGGER authored_head_integration_principal_matches_proposal
BEFORE INSERT ON authored_head_integrations FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM authored_head_proposals proposal
    WHERE proposal.proposal_id = NEW.proposal_id
      AND proposal.document_id = NEW.document_id
      AND proposal.principal_key = NEW.principal_key
)
BEGIN SELECT RAISE(ABORT, 'authored integration does not match proposal principal'); END;

CREATE INDEX idx_authored_head_integrations_server
    ON authored_head_integrations(server_integration_seq);

CREATE TRIGGER authored_head_integration_is_immutable
BEFORE UPDATE ON authored_head_integrations FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored head integration is immutable'); END;

CREATE TRIGGER authored_head_integration_is_permanent
BEFORE DELETE ON authored_head_integrations FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored head integration is permanent'); END;

-- Replace the checkpoint's Git-ledger projection guards with guards rooted in
-- terminal relational archive facts.
DROP TRIGGER IF EXISTS prevent_archived_score_resurrection;
DROP TRIGGER IF EXISTS prevent_archived_pattern_resurrection;
DROP TRIGGER IF EXISTS require_archived_score_before_delete;
DROP TRIGGER IF EXISTS require_archived_pattern_before_delete;
DROP TRIGGER IF EXISTS require_archived_implementation_before_delete;

CREATE TABLE authored_document_archives (
    archive_id            TEXT PRIMARY KEY,
    principal_key         TEXT NOT NULL,
    document_id           TEXT NOT NULL,
    device_id             TEXT NOT NULL,
    operation_id          TEXT NOT NULL,
    requested_revision_id TEXT,
    final_revision_id     TEXT,
    server_archive_seq    INTEGER,
    archived_at           TEXT NOT NULL,
    FOREIGN KEY (document_id, requested_revision_id)
        REFERENCES authored_revisions(document_id, revision_id),
    FOREIGN KEY (document_id, final_revision_id)
        REFERENCES authored_revisions(document_id, revision_id),
    UNIQUE (principal_key, document_id, device_id, operation_id)
);

CREATE TRIGGER authored_archive_principal_matches_document
BEFORE INSERT ON authored_document_archives FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_id = NEW.document_id
      AND document.principal_key = NEW.principal_key
)
OR (
    (SELECT remote_writes FROM auth_write_admission WHERE singleton = 1) = 0
    AND (
        NEW.final_revision_id IS NOT NULL
        OR NEW.server_archive_seq IS NOT NULL
        OR NOT EXISTS (
            SELECT 1 FROM authored_documents document
            WHERE document.document_id = NEW.document_id
              AND document.principal_key = NEW.principal_key
              AND document.archived_at IS NOT NULL
        )
        OR (
            NEW.requested_revision_id IS NULL
            AND (
                EXISTS (
                    SELECT 1 FROM authored_revisions revision
                    WHERE revision.document_id = NEW.document_id
                )
                OR EXISTS (
                    SELECT 1 FROM authored_document_heads head
                    WHERE head.document_id = NEW.document_id
                )
            )
        )
        OR (
            NEW.requested_revision_id IS NOT NULL
            AND NOT EXISTS (
                SELECT 1 FROM authored_document_heads head
                WHERE head.document_id = NEW.document_id
                  AND head.revision_id = NEW.requested_revision_id
            )
        )
    )
)
BEGIN SELECT RAISE(ABORT, 'authored archive principal does not own document'); END;

CREATE TRIGGER authored_document_archive_is_immutable
BEFORE UPDATE ON authored_document_archives FOR EACH ROW
WHEN NEW.archive_id IS NOT OLD.archive_id
  OR NEW.principal_key IS NOT OLD.principal_key
  OR NEW.document_id IS NOT OLD.document_id
  OR NEW.device_id IS NOT OLD.device_id
  OR NEW.operation_id IS NOT OLD.operation_id
  OR NEW.requested_revision_id IS NOT OLD.requested_revision_id
  OR NEW.archived_at IS NOT OLD.archived_at
  OR OLD.server_archive_seq IS NOT NULL
  OR NEW.server_archive_seq IS NULL
BEGIN SELECT RAISE(ABORT, 'authored document archive is immutable'); END;

CREATE TRIGGER authored_document_archive_is_permanent
BEFORE DELETE ON authored_document_archives FOR EACH ROW
BEGIN SELECT RAISE(ABORT, 'authored document archive is permanent'); END;
-- Archived authored documents are terminal. Canonical revisions remain
-- permanent, while catalog/live projection rows may be removed only after the
-- archive fact and document terminal timestamp are committed.

CREATE TRIGGER prevent_archived_score_resurrection
BEFORE INSERT ON scores FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_kind = 'track_score'
      AND document.score_id = NEW.id
      AND document.archived_at IS NOT NULL
)
BEGIN SELECT RAISE(ABORT, 'cannot recreate an archived authored score'); END;

CREATE TRIGGER prevent_archived_pattern_resurrection
BEFORE INSERT ON patterns FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_kind = 'pattern_graph'
      AND document.subject_id = NEW.id
      AND document.archived_at IS NOT NULL
)
BEGIN SELECT RAISE(ABORT, 'cannot recreate an archived authored pattern'); END;

CREATE TRIGGER require_archived_score_before_delete
BEFORE DELETE ON scores FOR EACH ROW
WHEN (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_kind = 'track_score'
      AND document.score_id = OLD.id
      AND NOT EXISTS (
          SELECT 1 FROM authored_document_archives archive
          WHERE archive.document_id = document.document_id
      )
 )
BEGIN SELECT RAISE(ABORT, 'score deletion requires a terminal authored archive'); END;

CREATE TRIGGER require_archived_pattern_before_delete
BEFORE DELETE ON patterns FOR EACH ROW
WHEN (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_kind = 'pattern_graph'
      AND document.subject_id = OLD.id
      AND NOT EXISTS (
          SELECT 1 FROM authored_document_archives archive
          WHERE archive.document_id = document.document_id
      )
 )
BEGIN SELECT RAISE(ABORT, 'pattern deletion requires terminal authored archives'); END;

CREATE TRIGGER require_archived_implementation_before_delete
BEFORE DELETE ON implementations FOR EACH ROW
WHEN (SELECT maintenance FROM auth_write_admission WHERE singleton = 1) = 0
 AND EXISTS (
    SELECT 1 FROM authored_documents document
    WHERE document.document_kind = 'pattern_graph'
      AND document.subject_id = OLD.pattern_id
      AND document.implementation_id = OLD.id
      AND NOT EXISTS (
          SELECT 1 FROM authored_document_archives archive
          WHERE archive.document_id = document.document_id
      )
 )
BEGIN SELECT RAISE(ABORT, 'implementation deletion requires a terminal authored archive'); END;
-- A completed thread deletion removes the lifecycle row which made retries
-- distinguishable from unknown IDs. Preserve one exact-owner terminal receipt
-- in the same transaction as the delete so uncertain responses are safely
-- replayable without rerunning external cleanup.
CREATE TABLE agent_thread_deletions (
    thread_id      TEXT PRIMARY KEY,
    owner_user_id  TEXT,
    principal_key  TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    document_id    TEXT NOT NULL,
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
    JOIN authored_documents document ON document.document_id = NEW.document_id
    CROSS JOIN auth_write_admission admission
    WHERE thread.id = NEW.thread_id
      AND thread.owner_user_id IS NEW.owner_user_id
      AND NEW.principal_key = CASE
            WHEN NEW.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || NEW.owner_user_id
          END
      AND thread.lifecycle_state = 'deleting'
      AND document.principal_key = NEW.principal_key
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
      AND admission.singleton = 1
      AND (
          admission.armed = 0
          OR (
              admission.accepting = 1
              AND admission.maintenance = 0
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

-- Admission triggers already existed on the checkpoint thread table; recreate
-- them so immutable fork routing participates in the same principal boundary.
DROP TRIGGER agent_thread_insert_requires_active_admission;
DROP TRIGGER agent_thread_routing_identity_is_immutable;
DROP TRIGGER agent_thread_update_requires_active_admission;
DROP TRIGGER agent_thread_delete_requires_active_admission;
-- StateDb stores credentials; the app database's admission row is the sole
-- live authority for durable conversation state. Local writes require the
-- exact active thread. A trusted pull (`remote_writes = 1`) may hydrate an
-- immutable trace after its lifecycle row has been terminally deleted; the
-- row's principal and every parent/range invariant still apply.

CREATE TRIGGER agent_thread_insert_requires_active_admission
BEFORE INSERT ON agent_threads FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
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
  OR NEW.forked_from_thread_id IS NOT OLD.forked_from_thread_id
  OR NEW.forked_at_message_id IS NOT OLD.forked_at_message_id
  OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'agent thread routing identity is immutable'); END;

CREATE TRIGGER agent_thread_update_requires_active_admission
BEFORE UPDATE ON agent_threads FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND OLD.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent thread write lacks active principal admission'); END;

CREATE TRIGGER agent_thread_delete_requires_active_admission
BEFORE DELETE ON agent_threads FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND OLD.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent thread write lacks active principal admission'); END;

-- A remote trace can retain a graph route after its catalog projection has
-- been archived or removed. Normal local creation still requires the concrete
-- implementation to exist.
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
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread requires an exact track or pattern authored route');
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
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread requires an exact track or pattern authored route');
END;

DROP TRIGGER agent_thread_message_requires_valid_parent;
CREATE TRIGGER agent_thread_message_requires_valid_parent
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN (
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
    SELECT RAISE(ABORT, 'agent transcript message has an invalid parent or principal');
END;

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
          admission.remote_writes = 1
          OR EXISTS (
              SELECT 1 FROM agent_threads AS thread
              WHERE thread.id = NEW.created_in_thread_id
                AND thread.owner_user_id IS NEW.owner_user_id
                AND thread.lifecycle_state = 'active'
          )
      )
)
BEGIN SELECT RAISE(ABORT, 'agent thread child write lacks owner admission'); END;

CREATE TRIGGER agent_thread_message_delete_requires_owner_admission
BEFORE DELETE ON agent_thread_messages FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND OLD.owner_user_id IS admission.active_uid
      AND OLD.principal_key = CASE
            WHEN OLD.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || OLD.owner_user_id
          END
)
BEGIN SELECT RAISE(ABORT, 'agent thread child write lacks owner admission'); END;

CREATE TRIGGER agent_thread_head_insert_requires_owner_admission
BEFORE INSERT ON agent_thread_transcript_heads FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads AS thread
    CROSS JOIN auth_write_admission AS admission
    WHERE thread.id = NEW.thread_id
      AND NEW.owner_user_id IS thread.owner_user_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent transcript head write lacks owner admission'); END;

CREATE TRIGGER agent_thread_head_update_requires_owner_admission
BEFORE UPDATE ON agent_thread_transcript_heads FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM agent_threads AS thread
    CROSS JOIN auth_write_admission AS admission
    WHERE thread.id = OLD.thread_id
      AND OLD.owner_user_id IS thread.owner_user_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent transcript head write lacks owner admission'); END;

CREATE TRIGGER agent_thread_head_delete_requires_owner_admission
BEFORE DELETE ON agent_thread_transcript_heads FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM agent_threads WHERE id = OLD.thread_id)
AND NOT EXISTS (
    SELECT 1 FROM agent_threads AS thread
    CROSS JOIN auth_write_admission AS admission
    WHERE thread.id = OLD.thread_id
      AND OLD.owner_user_id IS thread.owner_user_id
      AND admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND thread.owner_user_id IS admission.active_uid
)
BEGIN SELECT RAISE(ABORT, 'agent transcript head write lacks owner admission'); END;

DROP TRIGGER agent_thread_append_requires_active_thread;
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
        UNION ALL
        SELECT parent.id, parent.parent_message_id
        FROM agent_thread_messages AS parent
        JOIN lineage AS child ON child.parent_message_id = parent.id
    )
    SELECT 1
    FROM lineage
    JOIN agent_thread_messages AS first ON first.id = NEW.first_message_id
    JOIN agent_thread_messages AS result ON result.id = NEW.result_head_message_id
    WHERE lineage.id = first.id
      AND first.owner_user_id IS NEW.owner_user_id
      AND first.principal_key = NEW.principal_key
      AND first.parent_message_id IS NEW.base_head_message_id
      AND result.depth - first.depth + 1 = NEW.message_count
)
BEGIN
    SELECT RAISE(ABORT, 'agent thread append receipt has invalid scope or range');
END;

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
          admission.remote_writes = 1
          OR EXISTS (
              SELECT 1 FROM agent_threads AS thread
              WHERE thread.id = NEW.thread_id
                AND thread.owner_user_id IS NEW.owner_user_id
                AND thread.lifecycle_state = 'active'
          )
      )
)
BEGIN SELECT RAISE(ABORT, 'agent thread child write lacks owner admission'); END;

CREATE TRIGGER agent_thread_append_delete_requires_owner_admission
BEFORE DELETE ON agent_thread_message_appends FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1
      AND admission.accepting = 1 AND admission.maintenance = 0
      AND OLD.owner_user_id IS admission.active_uid
      AND OLD.principal_key = CASE
            WHEN OLD.owner_user_id IS NULL THEN 'signed-out'
            ELSE 'signed-in:' || OLD.owner_user_id
          END
)
BEGIN SELECT RAISE(ABORT, 'agent thread child write lacks owner admission'); END;

DROP TRIGGER authored_turn_preparation_requires_active_thread;
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
BEGIN SELECT RAISE(ABORT, 'authored turn preparation lacks admitted document scope'); END;

DROP TRIGGER authored_turn_outcome_requires_active_thread;
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
)
BEGIN SELECT RAISE(ABORT, 'authored turn outcome lacks its persisted assistant'); END;

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
          admission.remote_writes = 1
          OR EXISTS (
              SELECT 1 FROM agent_threads AS thread
              WHERE thread.id = NEW.thread_id
                AND thread.owner_user_id IS NEW.owner_user_id
                AND thread.lifecycle_state = 'active'
          )
      )
)
BEGIN SELECT RAISE(ABORT, 'authored turn outcome lacks owner admission'); END;
-- A pending operation is authority-bearing durable state. Bind it permanently
-- to the app-database principal admitted when it was enqueued, so a later
-- signed-out/signed-in or signed-in/signed-in identity switch can neither
-- flush nor mutate it.
-- `signed-out` and `signed-in:<uid>` are disjoint non-null identities; NULL cannot be
-- used because SQLite uniqueness treats NULL values as distinct.

CREATE TABLE pending_ops_principalized (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    principal_key TEXT NOT NULL
        CHECK (
            principal_key = 'signed-out'
            OR (substr(principal_key, 1, 10) = 'signed-in:' AND length(principal_key) > 10)
        ),
    op_type TEXT NOT NULL CHECK(op_type IN (
        'upsert', 'delete', 'insert_immutable', 'upsert_explicit',
        'submit_authored_head_proposal', 'integrate_authored_head_proposal',
        'archive_authored_document'
    )),
    table_name TEXT NOT NULL,
    record_id TEXT NOT NULL,
    payload_json TEXT,
    conflict_key TEXT NOT NULL DEFAULT 'id',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    next_retry_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Existing upserts normally carry their owner in the immutable queued
-- payload. Old tombstones did not, so bind those to the admission that owns
-- this database at migration time; a closed or signed-out database keeps them
-- in the non-flushable signed-out queue.
INSERT INTO pending_ops_principalized (
    id, principal_key, op_type, table_name, record_id, payload_json,
    conflict_key, attempts, last_error, created_at, next_retry_at
)
SELECT
    op.id,
    CASE
        WHEN op.payload_json IS NOT NULL AND json_valid(op.payload_json) THEN
            CASE
                WHEN json_type(op.payload_json, '$.uid') = 'text'
                     AND length(json_extract(op.payload_json, '$.uid')) > 0
                    THEN 'signed-in:' || json_extract(op.payload_json, '$.uid')
                WHEN json_type(op.payload_json, '$.uid') = 'null'
                    THEN 'signed-out'
                ELSE COALESCE(
                    (
                        SELECT 'signed-in:' || admission.active_uid
                        FROM auth_write_admission AS admission
                        WHERE admission.singleton = 1
                          AND admission.armed = 1
                          AND admission.accepting = 1
                          AND admission.maintenance = 0
                          AND admission.remote_writes = 0
                          AND admission.active_uid IS NOT NULL
                    ),
                    'signed-out'
                )
            END
        ELSE COALESCE(
            (
                SELECT 'signed-in:' || admission.active_uid
                FROM auth_write_admission AS admission
                WHERE admission.singleton = 1
                  AND admission.armed = 1
                  AND admission.accepting = 1
                  AND admission.maintenance = 0
                  AND admission.remote_writes = 0
                  AND admission.active_uid IS NOT NULL
            ),
            'signed-out'
        )
    END,
    op.op_type,
    op.table_name,
    op.record_id,
    op.payload_json,
    op.conflict_key,
    op.attempts,
    op.last_error,
    op.created_at,
    op.next_retry_at
FROM pending_ops AS op;

-- SQLite validates trigger bodies while a referenced table is dropped, so
-- retire every live enqueue trigger before replacing the queue table. They are
-- recreated below against the principalized schema.
DROP TRIGGER IF EXISTS sync_delete_venues;
DROP TRIGGER IF EXISTS sync_delete_tracks;
DROP TRIGGER IF EXISTS sync_delete_pattern_categories;
DROP TRIGGER IF EXISTS sync_delete_fixtures;
DROP TRIGGER IF EXISTS sync_delete_patterns;
DROP TRIGGER IF EXISTS sync_delete_fixture_groups;
DROP TRIGGER IF EXISTS sync_delete_scores;
DROP TRIGGER IF EXISTS sync_delete_fixture_group_members;
DROP TRIGGER IF EXISTS sync_delete_midi_modifiers;
DROP TRIGGER IF EXISTS sync_delete_cues;
DROP TRIGGER IF EXISTS sync_delete_midi_bindings;

DROP TABLE pending_ops;
ALTER TABLE pending_ops_principalized RENAME TO pending_ops;

CREATE INDEX idx_pending_ops_next_retry
    ON pending_ops(principal_key, next_retry_at);
CREATE INDEX idx_pending_ops_table_record
    ON pending_ops(principal_key, table_name, record_id);
CREATE UNIQUE INDEX idx_pending_ops_dedup
    ON pending_ops(principal_key, table_name, record_id, op_type);

-- Every local tombstone captures the exact open admission in the same SQLite
-- statement as enqueue. Identity switches and maintenance close that admission
-- before deleting anything, so their cascades cannot leak into another queue.

DROP TRIGGER IF EXISTS sync_delete_venues;
CREATE TRIGGER sync_delete_venues AFTER DELETE ON venues FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'venues', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_tracks;
CREATE TRIGGER sync_delete_tracks AFTER DELETE ON tracks FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'tracks', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_pattern_categories;
CREATE TRIGGER sync_delete_pattern_categories AFTER DELETE ON pattern_categories FOR EACH ROW
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'pattern_categories', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_fixtures;
CREATE TRIGGER sync_delete_fixtures AFTER DELETE ON fixtures FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'fixtures', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_patterns;
CREATE TRIGGER sync_delete_patterns AFTER DELETE ON patterns FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'patterns', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_fixture_groups;
CREATE TRIGGER sync_delete_fixture_groups AFTER DELETE ON fixture_groups FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'fixture_groups', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_scores;
CREATE TRIGGER sync_delete_scores AFTER DELETE ON scores FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'scores', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_fixture_group_members;
CREATE TRIGGER sync_delete_fixture_group_members AFTER DELETE ON fixture_group_members FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'fixture_group_members', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_midi_modifiers;
CREATE TRIGGER sync_delete_midi_modifiers AFTER DELETE ON midi_modifiers FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'midi_modifiers', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_cues;
CREATE TRIGGER sync_delete_cues AFTER DELETE ON cues FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'cues', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;

DROP TRIGGER IF EXISTS sync_delete_midi_bindings;
CREATE TRIGGER sync_delete_midi_bindings AFTER DELETE ON midi_bindings FOR EACH ROW
WHEN OLD.origin = 'local'
BEGIN
    INSERT INTO pending_ops (principal_key, op_type, table_name, record_id, next_retry_at)
    SELECT CASE WHEN admission.active_uid IS NULL THEN 'signed-out' ELSE 'signed-in:' || admission.active_uid END,
           'delete', 'midi_bindings', OLD.id, CURRENT_TIMESTAMP
    FROM auth_write_admission AS admission
    WHERE admission.singleton = 1 AND admission.armed = 1 AND admission.accepting = 1
      AND admission.maintenance = 0 AND admission.remote_writes = 0
    ON CONFLICT(principal_key, table_name, record_id, op_type) DO UPDATE SET
        attempts = 0, last_error = NULL, next_retry_at = CURRENT_TIMESTAMP;
END;
-- Every newly persisted assistant response is one authored turn. The exact
-- score/graph revision is reserved first in authored_turn_preparations, then the
-- append-only transcript may claim that message identity. Existing legacy
-- rows are intentionally untouched; this closes the insertion path going
-- forward without inventing authored history for old conversations.
CREATE TRIGGER assistant_message_requires_prepared_authored_turn
BEFORE INSERT ON agent_thread_messages FOR EACH ROW
WHEN NEW.role = 'assistant'
 AND NOT EXISTS (
    SELECT 1
    FROM authored_turn_preparations AS authored_turn
    JOIN agent_threads AS thread
      ON thread.id = authored_turn.thread_id
    WHERE authored_turn.thread_id = NEW.created_in_thread_id
      AND authored_turn.assistant_message_id = NEW.id
      AND authored_turn.owner_user_id IS NEW.owner_user_id
      AND authored_turn.principal_key = NEW.principal_key
      AND thread.lifecycle_state = 'active'
      AND NOT EXISTS (
          SELECT 1 FROM authored_turn_outcomes AS outcome
          WHERE outcome.thread_id = authored_turn.thread_id
            AND outcome.assistant_message_id = authored_turn.assistant_message_id
      )
 )
BEGIN
    SELECT RAISE(ABORT, 'assistant message requires a prepared authored turn');
END;
