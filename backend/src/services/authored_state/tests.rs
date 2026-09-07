use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Connection, SqliteConnection, SqlitePool};
use tempfile::TempDir;

use super::*;

struct Fixture {
    connection: SqliteConnection,
    store: AuthoredRevisionStore,
    document: NewAuthoredDocument,
}

impl Fixture {
    async fn new() -> Self {
        let mut connection = test_connection().await;
        let store = AuthoredRevisionStore;
        let document =
            NewAuthoredDocument::track_score("signed-in:user-a", "track-a", "venue-a", "score-a")
                .unwrap();
        store
            .insert_document(&mut connection, &document)
            .await
            .unwrap();
        Self {
            connection,
            store,
            document,
        }
    }

    async fn revision(
        &mut self,
        parents: &[RevisionId],
        files: &FileMap,
        metadata: &RevisionMetadata,
    ) -> RevisionInfo {
        self.store
            .insert_revision(
                &mut self.connection,
                &self.document.id,
                parents,
                files,
                metadata,
            )
            .await
            .unwrap()
    }

    async fn root(&mut self, files: &FileMap) -> RevisionInfo {
        self.revision(&[], files, &metadata("initialize", "root"))
            .await
    }
}

async fn test_connection() -> SqliteConnection {
    let mut connection = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::migrate!("./migrations")
        .run(&mut connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut connection)
        .await
        .unwrap();
    connection
}

async fn test_pool() -> (TempDir, SqlitePool) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("authored-state.sqlite");
    let migrate_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .unwrap();
    sqlx::migrate!("./migrations")
        .run(&migrate_pool)
        .await
        .unwrap();
    migrate_pool.close().await;
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    (directory, pool)
}

fn metadata(operation_kind: &str, operation_id: &str) -> RevisionMetadata {
    RevisionMetadata {
        operation_kind: operation_kind.into(),
        operation_id: Some(operation_id.into()),
        message: format!("{operation_kind} {operation_id}"),
        actor: Actor::user(),
        author_name: "Luma test".into(),
        author_email: "test@luma.local".into(),
        authored_at: "2026-08-02T12:00:00Z".into(),
        thread_id: None,
        assistant_message_id: None,
        restored_revision_id: None,
    }
}

fn file_map<const N: usize>(entries: [(&str, &[u8]); N]) -> FileMap {
    entries
        .into_iter()
        .map(|(path, bytes)| (path.to_owned(), bytes.to_vec()))
        .collect()
}

fn one_file(contents: &[u8]) -> FileMap {
    file_map([("score.luma", contents)])
}

#[tokio::test(flavor = "current_thread")]
async fn document_ids_bind_kind_scope_and_principal() {
    let first =
        NewAuthoredDocument::track_score("signed-in:user-a", "track-a", "venue-a", "score-a")
            .unwrap();
    let same =
        NewAuthoredDocument::track_score("signed-in:user-a", "track-a", "venue-a", "score-a")
            .unwrap();
    let other_principal =
        NewAuthoredDocument::track_score("signed-in:user-b", "track-a", "venue-a", "score-a")
            .unwrap();
    let graph =
        NewAuthoredDocument::pattern_graph("signed-in:user-a", "track-a", "implementation-a")
            .unwrap();

    assert_eq!(first.id, same.id);
    assert_ne!(first.id, other_principal.id);
    assert_ne!(first.id, graph.id);
    assert_eq!(
        AuthoredDocumentId::parse(first.id.to_string()).unwrap(),
        first.id
    );
    for invalid in ["../escape", "ad-123", "rv-deadbeef", ""] {
        assert!(AuthoredDocumentId::parse(invalid).is_err());
    }

    let mut connection = test_connection().await;
    let store = AuthoredRevisionStore;
    let inserted = store
        .insert_document(&mut connection, &first)
        .await
        .unwrap();
    let retry = store
        .insert_document(&mut connection, &first)
        .await
        .unwrap();
    assert_eq!(inserted, retry);
    assert_eq!(inserted.spec.principal_key, "signed-in:user-a");
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_catalog_routes_reject_new_document_identities_atomically() {
    let mut connection = test_connection().await;
    let store = AuthoredRevisionStore;

    let score =
        NewAuthoredDocument::track_score("signed-in:user-a", "track-a", "venue-a", "score-a")
            .unwrap();
    store
        .insert_document(&mut connection, &score)
        .await
        .unwrap();
    let score_root = store
        .insert_revision(
            &mut connection,
            &score.id,
            &[],
            &one_file(b"score root"),
            &metadata("initialize", "terminal-score-root"),
        )
        .await
        .unwrap();
    store
        .create_head(&mut connection, &score.id, &score_root.id)
        .await
        .unwrap();
    store
        .archive_document(
            &mut connection,
            &score.id,
            &score_root.id,
            "2026-08-02T13:00:00Z",
        )
        .await
        .unwrap();

    // An exact response-loss retry still resolves the permanent row, but a
    // different deterministic identity cannot resurrect the archived route.
    let retry = store
        .insert_document(&mut connection, &score)
        .await
        .unwrap();
    assert_eq!(retry.archived_at.as_deref(), Some("2026-08-02T13:00:00Z"));
    let replacement_score =
        NewAuthoredDocument::track_score("signed-in:user-a", "track-b", "venue-b", "score-a")
            .unwrap();
    let score_error = store
        .insert_document(&mut connection, &replacement_score)
        .await
        .unwrap_err();
    assert!(matches!(score_error, AuthoredStateError::InvalidInput(_)));

    let graph =
        NewAuthoredDocument::pattern_graph("signed-in:user-a", "pattern-a", "implementation-a")
            .unwrap();
    store
        .insert_document(&mut connection, &graph)
        .await
        .unwrap();
    let graph_root = store
        .insert_revision(
            &mut connection,
            &graph.id,
            &[],
            &file_map([("graph.json", b"{}".as_slice())]),
            &metadata("initialize", "terminal-graph-root"),
        )
        .await
        .unwrap();
    store
        .create_head(&mut connection, &graph.id, &graph_root.id)
        .await
        .unwrap();
    store
        .archive_document(
            &mut connection,
            &graph.id,
            &graph_root.id,
            "2026-08-02T13:00:01Z",
        )
        .await
        .unwrap();

    let replacement_graph =
        NewAuthoredDocument::pattern_graph("signed-in:user-a", "pattern-a", "implementation-b")
            .unwrap();
    let graph_error = store
        .insert_document(&mut connection, &replacement_graph)
        .await
        .unwrap_err();
    assert!(matches!(graph_error, AuthoredStateError::InvalidInput(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn exact_bytes_round_trip_and_exact_revision_retry_is_idempotent() {
    let mut fixture = Fixture::new().await;
    let bytes = [0, 255, b'\n', b'#', 0, 128, b'\r', b'\n'];
    let files = file_map([
        ("score.luma", bytes.as_slice()),
        ("notes/comment.bin", [9, 8, 0, 7].as_slice()),
    ]);
    let metadata = metadata("initialize", "binary-root");
    let first = fixture.revision(&[], &files, &metadata).await;
    let retry = fixture.revision(&[], &files, &metadata).await;

    assert_eq!(first, retry);
    assert_eq!(first.principal_key, fixture.document.principal_key);
    let (read_info, read_files) = fixture
        .store
        .read_revision(&mut fixture.connection, &fixture.document.id, &first.id)
        .await
        .unwrap();
    assert_eq!(read_info, first);
    assert_eq!(read_files, files);
    let manifest = content_manifest(&files).unwrap();
    assert_eq!(manifest.content_hash, first.content_hash);
    assert_eq!(manifest.byte_length, 12);
}

#[tokio::test(flavor = "current_thread")]
async fn same_content_restore_is_a_distinct_forward_revision() {
    let mut fixture = Fixture::new().await;
    let original_files = one_file(b"# original\n");
    let changed_files = one_file(b"# changed\n");
    let original = fixture.root(&original_files).await;
    let changed = fixture
        .revision(
            std::slice::from_ref(&original.id),
            &changed_files,
            &metadata("edit", "change-one"),
        )
        .await;
    let mut restore_metadata = metadata("restore", "restore-original");
    restore_metadata.restored_revision_id = Some(original.id.clone());
    let restored = fixture
        .revision(
            std::slice::from_ref(&changed.id),
            &original_files,
            &restore_metadata,
        )
        .await;

    assert_eq!(original.content_hash, restored.content_hash);
    assert_ne!(original.id, restored.id);
    assert_eq!(restored.parents, vec![changed.id.clone()]);
    assert_eq!(
        restored.metadata.restored_revision_id,
        Some(original.id.clone())
    );
    assert_eq!(
        fixture
            .store
            .read_revision(&mut fixture.connection, &fixture.document.id, &restored.id,)
            .await
            .unwrap()
            .1,
        original_files
    );
}

#[tokio::test(flavor = "current_thread")]
async fn head_cas_is_forward_only_and_retries_do_not_increment_generation() {
    let mut fixture = Fixture::new().await;
    let root = fixture.root(&one_file(b"root")).await;
    let child = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"child"),
            &metadata("edit", "child"),
        )
        .await;
    let sibling = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"sibling"),
            &metadata("edit", "sibling"),
        )
        .await;

    let initial_head = fixture
        .store
        .create_head(&mut fixture.connection, &fixture.document.id, &root.id)
        .await
        .unwrap();
    assert_eq!(initial_head.generation, 0);
    assert_eq!(initial_head.principal_key, fixture.document.principal_key);

    let advanced = fixture
        .store
        .compare_and_swap_head(
            &mut fixture.connection,
            &fixture.document.id,
            &root.id,
            &child.id,
        )
        .await
        .unwrap();
    assert_eq!(advanced.generation, 1);

    let response_loss_retry = fixture
        .store
        .compare_and_swap_head(
            &mut fixture.connection,
            &fixture.document.id,
            &root.id,
            &child.id,
        )
        .await
        .unwrap();
    assert_eq!(response_loss_retry.generation, 1);
    let same_revision_noop = fixture
        .store
        .compare_and_swap_head(
            &mut fixture.connection,
            &fixture.document.id,
            &child.id,
            &child.id,
        )
        .await
        .unwrap();
    assert_eq!(same_revision_noop.generation, 1);

    let error = fixture
        .store
        .compare_and_swap_head(
            &mut fixture.connection,
            &fixture.document.id,
            &root.id,
            &sibling.id,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AuthoredStateError::HeadConflict { actual, .. } if actual == child.id.to_string()
    ));

    let integrated_noop_at_stale_expected = fixture
        .store
        .compare_and_swap_integrated_head(
            &mut fixture.connection,
            &fixture.document.id,
            &root.id,
            &root.id,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        integrated_noop_at_stale_expected,
        AuthoredStateError::HeadConflict { actual, .. } if actual == child.id.to_string()
    ));

    let rewind = fixture
        .store
        .compare_and_swap_head(
            &mut fixture.connection,
            &fixture.document.id,
            &child.id,
            &root.id,
        )
        .await
        .unwrap_err();
    assert!(matches!(rewind, AuthoredStateError::InvalidInput(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_head_cas_has_exactly_one_winner() {
    let (_directory, pool) = test_pool().await;
    let store = AuthoredRevisionStore;
    let document = NewAuthoredDocument::track_score(
        "signed-in:race-user",
        "track-race",
        "venue-race",
        "score-race",
    )
    .unwrap();
    let (root, left, right) = {
        let mut connection = pool.acquire().await.unwrap();
        store
            .insert_document(&mut connection, &document)
            .await
            .unwrap();
        let root = store
            .insert_revision(
                &mut connection,
                &document.id,
                &[],
                &one_file(b"root"),
                &metadata("initialize", "race-root"),
            )
            .await
            .unwrap();
        let left = store
            .insert_revision(
                &mut connection,
                &document.id,
                std::slice::from_ref(&root.id),
                &one_file(b"left"),
                &metadata("edit", "race-left"),
            )
            .await
            .unwrap();
        let right = store
            .insert_revision(
                &mut connection,
                &document.id,
                std::slice::from_ref(&root.id),
                &one_file(b"right"),
                &metadata("edit", "race-right"),
            )
            .await
            .unwrap();
        store
            .create_head(&mut connection, &document.id, &root.id)
            .await
            .unwrap();
        (root, left, right)
    };

    let left_pool = pool.clone();
    let left_document = document.id.clone();
    let left_root = root.id.clone();
    let left_target = left.id.clone();
    let left_attempt = async move {
        let mut connection = left_pool.acquire().await.unwrap();
        store
            .compare_and_swap_head(&mut connection, &left_document, &left_root, &left_target)
            .await
    };
    let right_pool = pool.clone();
    let right_document = document.id.clone();
    let right_root = root.id.clone();
    let right_target = right.id.clone();
    let right_attempt = async move {
        let mut connection = right_pool.acquire().await.unwrap();
        store
            .compare_and_swap_head(&mut connection, &right_document, &right_root, &right_target)
            .await
    };
    let (left_result, right_result) = tokio::join!(left_attempt, right_attempt);
    let successes = usize::from(left_result.is_ok()) + usize::from(right_result.is_ok());
    assert_eq!(successes, 1);
    let failure = if left_result.is_err() {
        left_result.unwrap_err()
    } else {
        right_result.unwrap_err()
    };
    assert!(matches!(failure, AuthoredStateError::HeadConflict { .. }));

    let mut connection = pool.acquire().await.unwrap();
    let head = store.head(&mut connection, &document.id).await.unwrap();
    assert_eq!(head.generation, 1);
    assert!(head.revision_id == left.id || head.revision_id == right.id);
}

#[tokio::test(flavor = "current_thread")]
async fn criss_cross_merge_bases_are_complete_sorted_and_never_chosen_arbitrarily() {
    let mut fixture = Fixture::new().await;
    let root = fixture.root(&one_file(b"root")).await;
    let left = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"left"),
            &metadata("edit", "left"),
        )
        .await;
    let right = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"right"),
            &metadata("edit", "right"),
        )
        .await;
    let left_merge = fixture
        .revision(
            &[left.id.clone(), right.id.clone()],
            &one_file(b"both"),
            &metadata("merge", "left-merge"),
        )
        .await;
    let right_merge = fixture
        .revision(
            &[right.id.clone(), left.id.clone()],
            &one_file(b"both"),
            &metadata("merge", "right-merge"),
        )
        .await;

    let bases = fixture
        .store
        .merge_bases(
            &mut fixture.connection,
            &fixture.document.id,
            &left_merge.id,
            &right_merge.id,
        )
        .await
        .unwrap();
    let mut expected = vec![left.id.clone(), right.id.clone()];
    expected.sort();
    assert_eq!(bases, expected);
    let error = fixture
        .store
        .merge_base(
            &mut fixture.connection,
            &fixture.document.id,
            &left_merge.id,
            &right_merge.id,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AuthoredStateError::AmbiguousMergeBase { candidates }
            if candidates == expected.iter().map(ToString::to_string).collect::<Vec<_>>()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn unrelated_partial_revision_does_not_poison_valid_ancestry() {
    let mut fixture = Fixture::new().await;
    let root = fixture.root(&one_file(b"root")).await;
    let left = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"left"),
            &metadata("edit", "reachable-left"),
        )
        .await;
    let right = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"right"),
            &metadata("edit", "reachable-right"),
        )
        .await;

    // Model an immutable row uploaded before its closure. It belongs to this
    // document but is not reachable from either valid proposal tip.
    let orphan = RevisionId::parse(format!("rv-{}", "f".repeat(64))).unwrap();
    sqlx::query(
        "INSERT INTO authored_revisions
         (revision_id, document_id, principal_key, parent_count, content_hash,
          operation_kind, message, author_name, author_email, authored_at)
         VALUES (?, ?, ?, 1, 'sha256:partial', 'edit', 'Partial upload',
                 'Luma test', 'test@luma.local', '2026-08-02T12:00:00Z')",
    )
    .bind(orphan.as_str())
    .bind(fixture.document.id.as_str())
    .bind(&fixture.document.principal_key)
    .execute(&mut fixture.connection)
    .await
    .unwrap();

    assert!(fixture
        .store
        .is_ancestor(
            &mut fixture.connection,
            &fixture.document.id,
            &root.id,
            &left.id,
        )
        .await
        .unwrap());
    assert!(!fixture
        .store
        .is_ancestor(
            &mut fixture.connection,
            &fixture.document.id,
            &right.id,
            &left.id,
        )
        .await
        .unwrap());
    assert_eq!(
        fixture
            .store
            .merge_base(
                &mut fixture.connection,
                &fixture.document.id,
                &left.id,
                &right.id,
            )
            .await
            .unwrap(),
        root.id
    );
}

#[tokio::test(flavor = "current_thread")]
async fn history_is_first_parent_ordered_and_rejects_off_mainline_cursors() {
    let mut fixture = Fixture::new().await;
    let root = fixture.root(&one_file(b"root")).await;
    let ours = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"ours"),
            &metadata("edit", "ours"),
        )
        .await;
    let theirs = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"theirs"),
            &metadata("edit", "theirs"),
        )
        .await;
    let merged = fixture
        .revision(
            &[ours.id.clone(), theirs.id.clone()],
            &one_file(b"merged"),
            &metadata("merge", "merged"),
        )
        .await;
    fixture
        .store
        .create_head(&mut fixture.connection, &fixture.document.id, &root.id)
        .await
        .unwrap();
    fixture
        .store
        .compare_and_swap_head(
            &mut fixture.connection,
            &fixture.document.id,
            &root.id,
            &merged.id,
        )
        .await
        .unwrap();

    let history = fixture
        .store
        .first_parent_log_from(&mut fixture.connection, &fixture.document.id, None, 10)
        .await
        .unwrap();
    assert_eq!(
        history
            .into_iter()
            .map(|revision| revision.id)
            .collect::<Vec<_>>(),
        vec![merged.id.clone(), ours.id.clone(), root.id.clone()]
    );
    assert!(!fixture
        .store
        .first_parent_contains(&mut fixture.connection, &fixture.document.id, &theirs.id,)
        .await
        .unwrap());
    assert!(fixture
        .store
        .first_parent_log_from(
            &mut fixture.connection,
            &fixture.document.id,
            Some(&theirs.id),
            10,
        )
        .await
        .is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn diff_reports_added_deleted_and_modified_paths_in_order() {
    let mut fixture = Fixture::new().await;
    let old_files = file_map([
        ("a.deleted", b"old".as_slice()),
        ("b.modified", b"before".as_slice()),
        ("d.same", b"same".as_slice()),
    ]);
    let new_files = file_map([
        ("b.modified", b"after".as_slice()),
        ("c.added", b"new".as_slice()),
        ("d.same", b"same".as_slice()),
    ]);
    let old = fixture.root(&old_files).await;
    let new = fixture
        .revision(
            std::slice::from_ref(&old.id),
            &new_files,
            &metadata("edit", "diff"),
        )
        .await;

    let changes = fixture
        .store
        .diff(
            &mut fixture.connection,
            &fixture.document.id,
            &old.id,
            &new.id,
        )
        .await
        .unwrap();
    assert_eq!(
        changes
            .iter()
            .map(|change| (change.path.as_str(), change.kind))
            .collect::<Vec<_>>(),
        vec![
            ("a.deleted", FileChangeKind::Deleted),
            ("b.modified", FileChangeKind::Modified),
            ("c.added", FileChangeKind::Added),
        ]
    );
    assert!(changes
        .iter()
        .all(|change| { change.old_content_hash.is_some() || change.new_content_hash.is_some() }));
}

#[tokio::test(flavor = "current_thread")]
async fn immutable_rows_principal_binding_and_archive_terminal_are_database_invariants() {
    let mut fixture = Fixture::new().await;
    let root = fixture.root(&one_file(b"root")).await;
    let child = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"child"),
            &metadata("edit", "immutable-child"),
        )
        .await;
    fixture
        .store
        .create_head(&mut fixture.connection, &fixture.document.id, &root.id)
        .await
        .unwrap();
    fixture
        .store
        .compare_and_swap_head(
            &mut fixture.connection,
            &fixture.document.id,
            &root.id,
            &child.id,
        )
        .await
        .unwrap();

    assert!(
        sqlx::query("UPDATE authored_revisions SET message = 'mutated' WHERE revision_id = ?")
            .bind(child.id.as_str())
            .execute(&mut fixture.connection)
            .await
            .is_err()
    );
    assert!(sqlx::query(
        "UPDATE authored_revision_files SET content = X'00' WHERE revision_id = ?"
    )
    .bind(child.id.as_str())
    .execute(&mut fixture.connection)
    .await
    .is_err());
    assert!(
        sqlx::query("DELETE FROM authored_revision_parents WHERE revision_id = ?")
            .bind(child.id.as_str())
            .execute(&mut fixture.connection)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM authored_revisions WHERE revision_id = ?")
            .bind(child.id.as_str())
            .execute(&mut fixture.connection)
            .await
            .is_err()
    );
    assert!(sqlx::query(
        "UPDATE authored_documents SET principal_key = 'signed-in:attacker' WHERE document_id = ?"
    )
    .bind(fixture.document.id.as_str())
    .execute(&mut fixture.connection)
    .await
    .is_err());
    assert!(sqlx::query("UPDATE authored_document_heads SET principal_key = 'signed-in:attacker' WHERE document_id = ?")
        .bind(fixture.document.id.as_str())
        .execute(&mut fixture.connection)
        .await
        .is_err());

    let archived = fixture
        .store
        .archive_document(
            &mut fixture.connection,
            &fixture.document.id,
            &child.id,
            "2026-08-02T13:00:00Z",
        )
        .await
        .unwrap();
    assert_eq!(
        archived.archived_at.as_deref(),
        Some("2026-08-02T13:00:00Z")
    );
    assert!(
        sqlx::query("UPDATE authored_documents SET archived_at = NULL WHERE document_id = ?")
            .bind(fixture.document.id.as_str())
            .execute(&mut fixture.connection)
            .await
            .is_err()
    );
    assert!(fixture
        .store
        .compare_and_swap_head(
            &mut fixture.connection,
            &fixture.document.id,
            &child.id,
            &child.id,
        )
        .await
        .is_err());

    let exact_retry = fixture
        .store
        .insert_revision(
            &mut fixture.connection,
            &fixture.document.id,
            std::slice::from_ref(&root.id),
            &one_file(b"child"),
            &metadata("edit", "immutable-child"),
        )
        .await
        .unwrap();
    assert_eq!(exact_retry.id, child.id);
}

#[tokio::test(flavor = "current_thread")]
async fn cross_document_parents_and_cross_principal_child_rows_are_rejected() {
    let mut fixture = Fixture::new().await;
    let root = fixture.root(&one_file(b"root")).await;
    let other =
        NewAuthoredDocument::track_score("signed-in:user-b", "track-b", "venue-b", "score-b")
            .unwrap();
    fixture
        .store
        .insert_document(&mut fixture.connection, &other)
        .await
        .unwrap();
    let other_root = fixture
        .store
        .insert_revision(
            &mut fixture.connection,
            &other.id,
            &[],
            &one_file(b"other"),
            &metadata("initialize", "other-root"),
        )
        .await
        .unwrap();

    let cross_parent = fixture
        .store
        .insert_revision(
            &mut fixture.connection,
            &fixture.document.id,
            std::slice::from_ref(&other_root.id),
            &one_file(b"bad"),
            &metadata("edit", "cross-parent"),
        )
        .await
        .unwrap_err();
    assert!(matches!(cross_parent, AuthoredStateError::NotFound(_)));

    let direct_wrong_principal = sqlx::query(
        "INSERT INTO authored_revision_files
         (revision_id, principal_key, path, content_hash, content)
         VALUES (?, 'signed-in:user-b', 'forged', 'sha256:00', X'00')",
    )
    .bind(root.id.as_str())
    .execute(&mut fixture.connection)
    .await;
    assert!(direct_wrong_principal.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn declared_parent_shape_prevents_late_ancestry_mutation() {
    let mut fixture = Fixture::new().await;
    let root = fixture.root(&one_file(b"root")).await;
    let child = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"child"),
            &metadata("edit", "shape-child"),
        )
        .await;

    let late_parent = sqlx::query(
        "INSERT INTO authored_revision_parents
         (principal_key, document_id, revision_id, parent_order, parent_revision_id)
         VALUES (?, ?, ?, 0, ?)",
    )
    .bind(&fixture.document.principal_key)
    .bind(fixture.document.id.as_str())
    .bind(root.id.as_str())
    .bind(child.id.as_str())
    .execute(&mut fixture.connection)
    .await;
    assert!(late_parent.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_parent_cardinality_and_cycles_fail_closed() {
    let mut fixture = Fixture::new().await;
    let root = fixture.root(&one_file(b"root")).await;
    let child = fixture
        .revision(
            std::slice::from_ref(&root.id),
            &one_file(b"child"),
            &metadata("edit", "malformed-child"),
        )
        .await;

    sqlx::query("DROP TRIGGER authored_revision_parent_is_permanent")
        .execute(&mut fixture.connection)
        .await
        .unwrap();
    sqlx::query("DELETE FROM authored_revision_parents WHERE revision_id = ?")
        .bind(child.id.as_str())
        .execute(&mut fixture.connection)
        .await
        .unwrap();
    let cardinality = fixture
        .store
        .is_ancestor(
            &mut fixture.connection,
            &fixture.document.id,
            &root.id,
            &child.id,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(cardinality, AuthoredStateError::Corrupt(message) if message.contains("declares 1 parents but stores 0"))
    );

    sqlx::query("DROP TRIGGER authored_revision_is_immutable")
        .execute(&mut fixture.connection)
        .await
        .unwrap();
    sqlx::query("UPDATE authored_revisions SET parent_count = 1 WHERE revision_id = ?")
        .bind(root.id.as_str())
        .execute(&mut fixture.connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO authored_revision_parents
         (principal_key, document_id, revision_id, parent_order, parent_revision_id)
         VALUES (?, ?, ?, 0, ?)",
    )
    .bind(&fixture.document.principal_key)
    .bind(fixture.document.id.as_str())
    .bind(root.id.as_str())
    .bind(child.id.as_str())
    .execute(&mut fixture.connection)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO authored_revision_parents
         (principal_key, document_id, revision_id, parent_order, parent_revision_id)
         VALUES (?, ?, ?, 0, ?)",
    )
    .bind(&fixture.document.principal_key)
    .bind(fixture.document.id.as_str())
    .bind(child.id.as_str())
    .bind(root.id.as_str())
    .execute(&mut fixture.connection)
    .await
    .unwrap();
    let cycle = fixture
        .store
        .is_ancestor(
            &mut fixture.connection,
            &fixture.document.id,
            &root.id,
            &child.id,
        )
        .await
        .unwrap_err();
    assert!(matches!(cycle, AuthoredStateError::Corrupt(message) if message.contains("cycle")));
}

#[tokio::test(flavor = "current_thread")]
async fn content_and_identity_corruption_are_detected_on_read() {
    let mut fixture = Fixture::new().await;
    let root = fixture.root(&one_file(b"root")).await;

    sqlx::query("DROP TRIGGER authored_revision_file_is_immutable")
        .execute(&mut fixture.connection)
        .await
        .unwrap();
    sqlx::query("UPDATE authored_revision_files SET content = X'00' WHERE revision_id = ?")
        .bind(root.id.as_str())
        .execute(&mut fixture.connection)
        .await
        .unwrap();
    let content_error = fixture
        .store
        .read_revision(&mut fixture.connection, &fixture.document.id, &root.id)
        .await
        .unwrap_err();
    assert!(
        matches!(content_error, AuthoredStateError::Corrupt(message) if message.contains("content hash mismatch"))
    );

    let mut identity_fixture = Fixture::new().await;
    let identity_root = identity_fixture.root(&one_file(b"root")).await;
    sqlx::query("DROP TRIGGER authored_revision_is_immutable")
        .execute(&mut identity_fixture.connection)
        .await
        .unwrap();
    sqlx::query("UPDATE authored_revisions SET message = 'forged' WHERE revision_id = ?")
        .bind(identity_root.id.as_str())
        .execute(&mut identity_fixture.connection)
        .await
        .unwrap();
    let identity_error = identity_fixture
        .store
        .read_revision(
            &mut identity_fixture.connection,
            &identity_fixture.document.id,
            &identity_root.id,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(identity_error, AuthoredStateError::Corrupt(message) if message.contains("deterministic id"))
    );
}

#[test]
fn canonical_paths_are_normalized_and_never_alias() {
    for invalid in [
        "",
        "/absolute",
        "../escape",
        "nested/../escape",
        "nested//file",
        "./file",
        "windows\\path",
        "drive:letter",
        "trailing./file ",
    ] {
        assert!(content_manifest(&file_map([(invalid, b"x".as_slice())])).is_err());
    }
    assert!(content_manifest(&file_map([("nested/score.luma", b"x".as_slice())])).is_ok());
}
