-- Recovery ledger for the real Git repositories under <app-config>/authored-state.
--
-- Git owns history, trees, refs, branches, and worktrees. SQLite records only
-- which Git main commit has been projected into the application's relational
-- model. A mutation prepares an immutable commit, atomically projects SQLite
-- and this ledger, then compare-and-swaps the Git ref. If the process dies
-- between projection and ref publication, the mismatch is enough to publish
-- the already-prepared descendant on startup/next access. Once a ledger row
-- exists, a mismatch between Git and the relational projection is corruption
-- and fails closed. Only a pre-existing document with no ledger may seed Git.
CREATE TABLE authored_state_projections (
    repository_id   TEXT PRIMARY KEY,
    document_kind   TEXT NOT NULL CHECK (document_kind IN ('track_score', 'pattern_graph')),
    principal_key   TEXT NOT NULL,
    subject_id      TEXT NOT NULL,
    track_id        TEXT,
    venue_id        TEXT,
    score_id        TEXT,
    implementation_id TEXT,
    implementation_name TEXT,
    projected_commit TEXT NOT NULL,
    materialization_state TEXT NOT NULL DEFAULT 'present'
        CHECK (materialization_state IN ('present', 'absent', 'archived')),
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    CHECK (
        (document_kind = 'track_score' AND track_id IS NOT NULL AND venue_id IS NOT NULL AND score_id IS NOT NULL AND implementation_id IS NULL AND implementation_name IS NULL)
        OR
        (document_kind = 'pattern_graph' AND track_id IS NULL AND venue_id IS NULL AND score_id IS NULL AND implementation_id IS NOT NULL)
    )
);

CREATE UNIQUE INDEX idx_authored_state_track_scope
    ON authored_state_projections(principal_key, track_id, venue_id, score_id)
    WHERE document_kind = 'track_score';

CREATE UNIQUE INDEX idx_authored_state_pattern_scope
    ON authored_state_projections(principal_key, subject_id, implementation_id)
    WHERE document_kind = 'pattern_graph';

CREATE TRIGGER authored_state_projections_updated_at
AFTER UPDATE ON authored_state_projections FOR EACH ROW
BEGIN
    UPDATE authored_state_projections
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
    WHERE repository_id = OLD.repository_id;
END;

-- Ownership/routing metadata only. The refs themselves live exclusively in
-- Git; these rows prevent one thread from naming another thread's branch or
-- linked worktree through an IPC payload.
CREATE TABLE authored_state_thread_branches (
    thread_id       TEXT PRIMARY KEY,
    repository_id   TEXT NOT NULL,
    branch_name     TEXT NOT NULL UNIQUE,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE INDEX idx_authored_state_thread_repo
    ON authored_state_thread_branches(repository_id);

-- Idempotency/audit metadata for the durable turn boundary. A preparation row
-- is written as soon as the thread branch commit exists; main_commit becomes
-- non-NULL in the same transaction as the relational projection. This
-- deliberately duplicates no tree or authored document bytes.
CREATE TABLE authored_state_turn_commits (
    thread_id            TEXT NOT NULL,
    assistant_message_id TEXT NOT NULL,
    repository_id        TEXT NOT NULL,
    branch_commit        TEXT NOT NULL,
    main_commit          TEXT,
    status               TEXT NOT NULL DEFAULT 'prepared'
        CHECK (status IN ('prepared', 'committed', 'conflicted')),
    conflicts_json       TEXT,
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    PRIMARY KEY (thread_id, assistant_message_id),
    UNIQUE (assistant_message_id),
    CHECK (
        (status = 'prepared' AND main_commit IS NULL AND conflicts_json IS NULL)
        OR (status = 'committed' AND main_commit IS NOT NULL AND conflicts_json IS NULL)
        OR (status = 'conflicted' AND main_commit IS NULL AND conflicts_json IS NOT NULL)
    )
);

CREATE UNIQUE INDEX idx_authored_state_turn_main_commit
    ON authored_state_turn_commits(repository_id, main_commit)
    WHERE main_commit IS NOT NULL;

-- Operation IDs are durable idempotency keys for every non-transcript edit and
-- main-history action. They are committed atomically with the projection
-- ledger, before the main ref CAS. `result_json` stores correlation metadata
-- for score edits only (draft-id mappings and change counts), never authored
-- document bytes.
CREATE TABLE authored_state_operations (
    repository_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL CHECK (
        operation_kind IN ('restore', 'worktree_merge', 'pattern_fork', 'score_edit', 'graph_edit')
    ),
    operation_id   TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    base_main_commit TEXT NOT NULL,
    commit_id      TEXT,
    status         TEXT NOT NULL CHECK (status IN ('committed', 'conflicted')),
    conflicts_json TEXT,
    result_json    TEXT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    PRIMARY KEY (repository_id, operation_kind, operation_id),
    CHECK (
        (status = 'committed' AND commit_id IS NOT NULL AND conflicts_json IS NULL)
        OR (status = 'conflicted' AND commit_id IS NULL AND conflicts_json IS NOT NULL)
    ),
    CHECK (
        (operation_kind = 'score_edit' AND status = 'committed' AND result_json IS NOT NULL)
        OR (operation_kind != 'score_edit' AND result_json IS NULL)
    )
);

CREATE UNIQUE INDEX idx_authored_state_operation_commit
    ON authored_state_operations(repository_id, commit_id)
    WHERE commit_id IS NOT NULL;

CREATE TABLE authored_state_worktrees (
    worktree_id      TEXT PRIMARY KEY,
    request_id       TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    repository_id    TEXT NOT NULL,
    owner_thread_id  TEXT NOT NULL,
    branch_name      TEXT NOT NULL UNIQUE,
    base_commit      TEXT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'preparing'
        CHECK (status IN ('preparing', 'active', 'retired')),
    created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    retired_at       TEXT
);

CREATE INDEX idx_authored_state_worktrees_owner
    ON authored_state_worktrees(owner_thread_id, status);

CREATE UNIQUE INDEX idx_authored_state_worktrees_request
    ON authored_state_worktrees(owner_thread_id, request_id);
