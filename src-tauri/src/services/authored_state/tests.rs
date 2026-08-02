use std::sync::{Arc, Barrier};
use std::thread;

use tempfile::TempDir;

use super::*;

struct Fixture {
    _directory: TempDir,
    store: AuthoredStateStore,
    repository_id: AuthoredRepositoryId,
    author: CommitAuthor,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let storage = StorageRoot::from_path(directory.path().join("luma"));
        let store = AuthoredStateStore::new(storage);
        let descriptor =
            AuthoredRepositoryDescriptor::track_score("user-a", "track-a", "venue-a", "score-a")
                .unwrap();
        let repository_id = AuthoredRepositoryId::derive(&descriptor);
        let author = CommitAuthor::new("Agent", "agent@luma.local", 1_700_000_000, 0).unwrap();
        Self {
            _directory: directory,
            store,
            repository_id,
            author,
        }
    }

    fn init(&self) -> RepositoryInfo {
        self.store.ensure_repository(&self.repository_id).unwrap()
    }

    fn commit(
        &self,
        branch: &str,
        expected: &CommitId,
        path: &str,
        contents: &str,
        message: &str,
    ) -> CommitInfo {
        self.store
            .commit_files(
                &self.repository_id,
                branch,
                expected,
                &file_map([(path, contents)]),
                &self.author,
                message,
            )
            .unwrap()
    }
}

fn file_map<const N: usize>(entries: [(&str, &str); N]) -> FileMap {
    entries
        .into_iter()
        .map(|(path, contents)| (path.to_string(), contents.as_bytes().to_vec()))
        .collect()
}

#[test]
fn repository_ids_are_account_and_scope_domain_separated() {
    let first = AuthoredRepositoryId::derive(
        &AuthoredRepositoryDescriptor::track_score("u", "t", "v", "s").unwrap(),
    );
    let same = AuthoredRepositoryId::derive(
        &AuthoredRepositoryDescriptor::track_score("u", "t", "v", "s").unwrap(),
    );
    let other_user = AuthoredRepositoryId::derive(
        &AuthoredRepositoryDescriptor::track_score("other", "t", "v", "s").unwrap(),
    );
    let graph = AuthoredRepositoryId::derive(
        &AuthoredRepositoryDescriptor::pattern_graph("u", "s", "i").unwrap(),
    );

    assert_eq!(first, same);
    assert_ne!(first, other_user);
    assert_ne!(first, graph);
    assert_eq!(
        AuthoredRepositoryId::parse(first.to_string()).unwrap(),
        first
    );
    for invalid in ["../escape", "r-123", "/tmp/repo", "r-xyz"] {
        assert!(AuthoredRepositoryId::parse(invalid).is_err());
    }
}

#[test]
fn initialize_is_atomic_idempotent_and_recoverable_from_a_new_store() {
    let fixture = Fixture::new();
    let first = fixture.init();
    assert!(first
        .path
        .ends_with(format!("{}.git", fixture.repository_id)));
    assert!(first.path.is_dir());
    let second = fixture.init();
    assert_eq!(first, second);

    let reopened = AuthoredStateStore::new(fixture.store.storage().clone());
    let recovered = reopened.ensure_repository(&fixture.repository_id).unwrap();
    assert_eq!(recovered.main_head, first.main_head);
    let (initial, files) = reopened
        .read_commit(&fixture.repository_id, &first.main_head)
        .unwrap();
    assert!(initial.parents.is_empty());
    assert_eq!(initial.message, INITIAL_MESSAGE);
    assert!(files.is_empty());

    let temporary_entries: Vec<_> =
        fs::read_dir(fixture.store.storage().authored_repositories_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".tmp-"))
            .collect();
    assert!(temporary_entries.is_empty());
}

#[test]
fn canonical_file_commits_round_trip_nested_paths_log_and_diff() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let first_files = file_map([
        ("state.json", "{\"version\":1}"),
        ("assets/palette.json", "[\"red\"]"),
    ]);
    let first = fixture
        .store
        .commit_files(
            &fixture.repository_id,
            MAIN_BRANCH,
            &initial,
            &first_files,
            &fixture.author,
            "first",
        )
        .unwrap();
    let second_files = file_map([
        ("state.json", "{\"version\":2}"),
        ("notes/readme.txt", "hello"),
    ]);
    let second = fixture
        .store
        .commit_files(
            &fixture.repository_id,
            MAIN_BRANCH,
            &first.id,
            &second_files,
            &fixture.author,
            "second",
        )
        .unwrap();

    assert_eq!(second.parents, vec![first.id.clone()]);
    assert_eq!(
        fixture
            .store
            .read_commit(&fixture.repository_id, &second.id)
            .unwrap()
            .1,
        second_files
    );
    assert_eq!(
        fixture
            .store
            .read_commit_file(&fixture.repository_id, &second.id, "notes/readme.txt")
            .unwrap(),
        b"hello"
    );

    let log = fixture
        .store
        .log(&fixture.repository_id, MAIN_BRANCH, 10)
        .unwrap();
    assert_eq!(log[0].id, second.id);
    assert_eq!(log[1].id, first.id);
    assert_eq!(log.last().unwrap().id, initial);

    let diff = fixture
        .store
        .diff(&fixture.repository_id, &first.id, &second.id)
        .unwrap();
    assert_eq!(
        diff.iter()
            .map(|entry| (entry.path.as_str(), entry.kind))
            .collect::<Vec<_>>(),
        vec![
            ("assets/palette.json", FileChangeKind::Deleted),
            ("notes/readme.txt", FileChangeKind::Added),
            ("state.json", FileChangeKind::Modified),
        ]
    );
}

#[test]
fn commit_retry_is_idempotent_but_stale_different_content_is_rejected() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let files = file_map([("state.json", "one")]);
    let first = fixture
        .store
        .commit_files(
            &fixture.repository_id,
            MAIN_BRANCH,
            &initial,
            &files,
            &fixture.author,
            "write one",
        )
        .unwrap();
    let retried = fixture
        .store
        .commit_files(
            &fixture.repository_id,
            MAIN_BRANCH,
            &initial,
            &files,
            &fixture.author,
            "write one",
        )
        .unwrap();
    assert_eq!(retried.id, first.id);

    let error = fixture
        .store
        .commit_files(
            &fixture.repository_id,
            MAIN_BRANCH,
            &initial,
            &file_map([("state.json", "different")]),
            &fixture.author,
            "different",
        )
        .unwrap_err();
    assert!(matches!(error, AuthoredStateError::HeadConflict { .. }));
    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, MAIN_BRANCH)
            .unwrap(),
        first.id
    );
}

#[test]
fn concurrent_writers_from_one_head_have_exactly_one_winner() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for value in ["left", "right"] {
        let store = fixture.store.clone();
        let repository_id = fixture.repository_id.clone();
        let expected = initial.clone();
        let author = fixture.author.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            store.commit_files(
                &repository_id,
                MAIN_BRANCH,
                &expected,
                &file_map([("state.json", value)]),
                &author,
                value,
            )
        }));
    }
    barrier.wait();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(AuthoredStateError::HeadConflict { .. })))
            .count(),
        1
    );
}

#[test]
fn tracked_paths_reject_traversal_git_control_paths_and_cross_platform_separators() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    for path in [
        "../escape",
        "/absolute",
        ".git/config",
        "nested/.GIT/config",
        ".luma-materialize-forged",
        "a\\windows",
        "a//empty",
        "state:stream",
        "CON",
        "trailing-dot.",
        "./dot",
        "trailing/",
    ] {
        let error = fixture
            .store
            .commit_files(
                &fixture.repository_id,
                MAIN_BRANCH,
                &initial,
                &BTreeMap::from([(path.to_string(), b"x".to_vec())]),
                &fixture.author,
                "unsafe",
            )
            .unwrap_err();
        assert!(matches!(error, AuthoredStateError::UnsafePath(_)), "{path}");
    }
    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, MAIN_BRANCH)
            .unwrap(),
        initial
    );
}

#[test]
fn prepared_commits_publish_with_cas_and_first_parent_history_is_deterministic() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let base = fixture.commit(MAIN_BRANCH, &initial, "state.json", "base", "base");
    fixture
        .store
        .create_branch(&fixture.repository_id, "feature", &base.id)
        .unwrap();
    let feature = fixture.commit("feature", &base.id, "feature.json", "feature", "feature");

    let prepared = fixture
        .store
        .prepare_commit(
            &fixture.repository_id,
            std::slice::from_ref(&base.id),
            &file_map([("state.json", "prepared")]),
            &fixture.author,
            "prepared",
        )
        .unwrap();
    let prepared_retry = fixture
        .store
        .prepare_commit(
            &fixture.repository_id,
            std::slice::from_ref(&base.id),
            &file_map([("state.json", "prepared")]),
            &fixture.author,
            "prepared",
        )
        .unwrap();
    assert_eq!(prepared_retry.id, prepared.id);
    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, MAIN_BRANCH)
            .unwrap(),
        base.id
    );

    fixture
        .store
        .advance_branch(&fixture.repository_id, MAIN_BRANCH, &base.id, &prepared.id)
        .unwrap();
    // A caller recovering after an ambiguous ref-update result can retry.
    fixture
        .store
        .advance_branch(&fixture.repository_id, MAIN_BRANCH, &base.id, &prepared.id)
        .unwrap();

    let next = fixture
        .store
        .prepare_commit(
            &fixture.repository_id,
            std::slice::from_ref(&prepared.id),
            &file_map([("state.json", "next")]),
            &fixture.author,
            "next",
        )
        .unwrap();
    let stale = fixture
        .store
        .advance_branch(&fixture.repository_id, MAIN_BRANCH, &base.id, &next.id)
        .unwrap_err();
    assert!(matches!(stale, AuthoredStateError::HeadConflict { .. }));
    fixture
        .store
        .advance_branch(&fixture.repository_id, MAIN_BRANCH, &prepared.id, &next.id)
        .unwrap();

    let non_fast_forward = fixture
        .store
        .advance_branch(&fixture.repository_id, MAIN_BRANCH, &next.id, &feature.id)
        .unwrap_err();
    assert!(matches!(
        non_fast_forward,
        AuthoredStateError::InvalidInput(_)
    ));

    let merged = fixture
        .store
        .prepare_commit(
            &fixture.repository_id,
            &[next.id.clone(), feature.id.clone()],
            &file_map([("feature.json", "feature"), ("state.json", "next")]),
            &fixture.author,
            "merge prepared",
        )
        .unwrap();
    assert_eq!(merged.parents, vec![next.id.clone(), feature.id.clone()]);
    fixture
        .store
        .advance_branch(&fixture.repository_id, MAIN_BRANCH, &next.id, &merged.id)
        .unwrap();

    let first_parent = fixture
        .store
        .first_parent_log(&fixture.repository_id, MAIN_BRANCH, 10)
        .unwrap();
    assert_eq!(
        first_parent
            .iter()
            .map(|commit| commit.id.clone())
            .collect::<Vec<_>>(),
        vec![merged.id, next.id, prepared.id, base.id, initial]
    );
    assert!(!first_parent.iter().any(|commit| commit.id == feature.id));
    assert!(fixture
        .store
        .first_parent_log(&fixture.repository_id, MAIN_BRANCH, 0)
        .is_err());
}

#[test]
fn branches_are_idempotent_and_never_force_moved() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "agent/topic", &initial)
        .unwrap();
    fixture
        .store
        .create_branch(&fixture.repository_id, "agent/topic", &initial)
        .unwrap();
    let advanced = fixture.commit(
        "agent/topic",
        &initial,
        "state.json",
        "branch",
        "advance branch",
    );
    let error = fixture
        .store
        .create_branch(&fixture.repository_id, "agent/topic", &initial)
        .unwrap_err();
    assert!(matches!(error, AuthoredStateError::HeadConflict { .. }));
    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, "agent/topic")
            .unwrap(),
        advanced.id
    );
    for invalid in ["", "../bad", "bad..name", "bad\\name", "-bad"] {
        assert!(fixture
            .store
            .create_branch(&fixture.repository_id, invalid, &initial)
            .is_err());
    }
}

#[test]
fn linked_branch_worktree_lifecycle_materializes_the_canonical_commit() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "agent-one", &initial)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-agent-one").unwrap();
    let worktree = fixture
        .store
        .create_worktree(&fixture.repository_id, "agent-one", &worktree_id)
        .unwrap();
    let retry = fixture
        .store
        .create_worktree(&fixture.repository_id, "agent-one", &worktree_id)
        .unwrap();
    assert_eq!(worktree, retry);
    assert_eq!(worktree.head, initial);
    assert!(worktree.path.join(".git").is_file());

    fs::create_dir_all(worktree.path.join("nested")).unwrap();
    fs::write(worktree.path.join("state.json"), b"state").unwrap();
    fs::write(worktree.path.join("nested/notes.txt"), b"notes").unwrap();
    fs::write(worktree.path.join("obsolete.txt"), b"remove me").unwrap();
    assert_eq!(
        fixture
            .store
            .read_worktree_files(&fixture.repository_id, &worktree_id, "agent-one")
            .unwrap(),
        file_map([
            ("nested/notes.txt", "notes"),
            ("obsolete.txt", "remove me"),
            ("state.json", "state"),
        ])
    );

    let source = file_map([
        ("nested/notes.txt", "notes"),
        ("obsolete.txt", "remove me"),
        ("state.json", "state"),
    ]);
    let canonical = file_map([
        ("nested/notes.txt", "canonical notes"),
        ("state.json", "canonical state"),
    ]);
    let committed = fixture
        .store
        .commit_worktree_files(
            &fixture.repository_id,
            &worktree_id,
            "agent-one",
            &initial,
            &source,
            &canonical,
            &fixture.author,
            "worktree commit",
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, "agent-one")
            .unwrap(),
        committed.id
    );
    assert_eq!(
        fixture
            .store
            .read_worktree_files(&fixture.repository_id, &worktree_id, "agent-one")
            .unwrap(),
        canonical
    );
    assert!(!worktree.path.join("obsolete.txt").exists());

    let listed = fixture
        .store
        .list_worktrees(&fixture.repository_id)
        .unwrap();
    assert_eq!(
        listed,
        vec![WorktreeInfo {
            head: committed.id.clone(),
            ..worktree
        }]
    );

    fixture
        .store
        .remove_worktree(&fixture.repository_id, &worktree_id, "agent-one", false)
        .unwrap();
    assert!(!retry.path.exists());
    assert!(fixture
        .store
        .list_worktrees(&fixture.repository_id)
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, "agent-one")
            .unwrap(),
        committed.id
    );
}

#[test]
fn canonical_materialization_handles_file_directory_shape_transitions() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let branch = "agent-shape-transition";
    fixture
        .store
        .create_branch(&fixture.repository_id, branch, &initial)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-shape-transition").unwrap();
    let worktree = fixture
        .store
        .create_worktree(&fixture.repository_id, branch, &worktree_id)
        .unwrap();

    fs::write(worktree.path.join("shape"), b"file").unwrap();
    let file_shape = file_map([("shape", "file")]);
    let directory_shape = file_map([("shape/value.txt", "nested")]);
    let first = fixture
        .store
        .commit_worktree_files(
            &fixture.repository_id,
            &worktree_id,
            branch,
            &initial,
            &file_shape,
            &directory_shape,
            &fixture.author,
            "file to directory",
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .read_worktree_files(&fixture.repository_id, &worktree_id, branch)
            .unwrap(),
        directory_shape
    );

    let file_again = file_map([("shape", "file again")]);
    fixture
        .store
        .commit_worktree_files(
            &fixture.repository_id,
            &worktree_id,
            branch,
            &first.id,
            &directory_shape,
            &file_again,
            &fixture.author,
            "directory to file",
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .read_worktree_files(&fixture.repository_id, &worktree_id, branch)
            .unwrap(),
        file_again
    );
}

#[test]
fn worktree_recovery_finishes_only_a_proven_source_canonical_mixture() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let branch = "agent-recovery";
    fixture
        .store
        .create_branch(&fixture.repository_id, branch, &initial)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-recovery").unwrap();
    let worktree = fixture
        .store
        .create_worktree(&fixture.repository_id, branch, &worktree_id)
        .unwrap();
    let source = file_map([
        ("graph.json", "source graph"),
        ("layout.json", "source layout"),
    ]);
    let canonical = file_map([
        ("graph.json", "canonical graph"),
        ("layout.json", "canonical layout"),
    ]);
    fs::write(worktree.path.join("graph.json"), &source["graph.json"]).unwrap();
    fs::write(worktree.path.join("layout.json"), &source["layout.json"]).unwrap();
    let source_manifest = worktree_source_manifest(&source).unwrap();
    let committed = fixture
        .store
        .commit_worktree_files(
            &fixture.repository_id,
            &worktree_id,
            branch,
            &initial,
            &source,
            &canonical,
            &fixture.author,
            "canonicalize graph",
        )
        .unwrap();

    // Model a process interruption after one of the two paths was replaced.
    fs::write(worktree.path.join("graph.json"), &source["graph.json"]).unwrap();
    let interrupted_temp = worktree.path.join(".luma-materialize-interrupted");
    fs::write(&interrupted_temp, b"partial canonical bytes").unwrap();
    assert!(fixture
        .store
        .recover_canonical_worktree_materialization(
            &fixture.repository_id,
            &worktree_id,
            branch,
            &committed.id,
            &source_manifest,
        )
        .unwrap());
    assert_eq!(
        fixture
            .store
            .read_worktree_files(&fixture.repository_id, &worktree_id, branch)
            .unwrap(),
        canonical
    );
    assert!(!interrupted_temp.exists());

    fs::write(worktree.path.join("graph.json"), b"newer edit").unwrap();
    fs::write(worktree.path.join("layout.json"), &source["layout.json"]).unwrap();
    assert!(!fixture
        .store
        .recover_canonical_worktree_materialization(
            &fixture.repository_id,
            &worktree_id,
            branch,
            &committed.id,
            &source_manifest,
        )
        .unwrap());
    assert_eq!(
        fs::read(worktree.path.join("graph.json")).unwrap(),
        b"newer edit"
    );
    assert_eq!(
        fs::read(worktree.path.join("layout.json")).unwrap(),
        source["layout.json"]
    );
}

#[test]
fn checkout_git_file_cannot_redirect_check_commit_or_materialization() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let branch = "agent-untrusted-gitfile";
    fixture
        .store
        .create_branch(&fixture.repository_id, branch, &initial)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-untrusted-gitfile").unwrap();
    let worktree = fixture
        .store
        .create_worktree(&fixture.repository_id, branch, &worktree_id)
        .unwrap();

    let outside = fixture._directory.path().join("attacker-worktree");
    let attacker_repo = Repository::init(&outside).unwrap();
    fs::write(outside.join("state.json"), b"outside sentinel").unwrap();
    let alternate_objects = attacker_repo.path().join("objects/info/alternates");
    fs::create_dir_all(alternate_objects.parent().unwrap()).unwrap();
    let bare_objects = fixture
        .store
        .storage()
        .authored_repository_dir(fixture.repository_id.as_str())
        .join("objects")
        .canonicalize()
        .unwrap();
    fs::write(alternate_objects, format!("{}\n", bare_objects.display())).unwrap();

    // The projection control file belongs to the agent-facing checkout. A
    // hostile replacement must not become a repository, ref, object, index,
    // or working-directory authority for any host operation.
    let malicious_gitfile = format!("gitdir: {}\n", attacker_repo.path().display());
    fs::write(worktree.path.join(".git"), &malicious_gitfile).unwrap();
    fs::write(worktree.path.join("state.json"), b"source").unwrap();
    let source = file_map([("state.json", "source")]);
    assert_eq!(
        fixture
            .store
            .read_worktree_files(&fixture.repository_id, &worktree_id, branch)
            .unwrap(),
        source
    );

    let canonical = file_map([
        ("nested/notes.txt", "canonical notes"),
        ("state.json", "canonical state"),
    ]);
    let committed = fixture
        .store
        .commit_worktree_files(
            &fixture.repository_id,
            &worktree_id,
            branch,
            &initial,
            &source,
            &canonical,
            &fixture.author,
            "ignore hostile checkout metadata",
        )
        .unwrap();

    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, branch)
            .unwrap(),
        committed.id
    );
    assert_eq!(
        fixture
            .store
            .read_worktree_files(&fixture.repository_id, &worktree_id, branch)
            .unwrap(),
        canonical
    );
    assert_eq!(
        fs::read(outside.join("state.json")).unwrap(),
        b"outside sentinel"
    );
    assert!(!outside.join("nested/notes.txt").exists());
    assert_eq!(
        fs::read_to_string(worktree.path.join(".git")).unwrap(),
        malicious_gitfile
    );
}

#[test]
fn switched_or_detached_worktree_cannot_escape_its_immutable_branch() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let branch = "agents/worktrees/thread/worktree";
    fixture
        .store
        .create_branch(&fixture.repository_id, branch, &initial)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-bound").unwrap();
    let worktree = fixture
        .store
        .create_worktree(&fixture.repository_id, branch, &worktree_id)
        .unwrap();
    let linked = Repository::open(&worktree.path).unwrap();

    linked.set_head("refs/heads/main").unwrap();
    for error in [
        fixture
            .store
            .read_worktree_files(&fixture.repository_id, &worktree_id, branch)
            .unwrap_err(),
        fixture
            .store
            .commit_worktree_files(
                &fixture.repository_id,
                &worktree_id,
                branch,
                &initial,
                &FileMap::new(),
                &file_map([("state.json", "escaped")]),
                &fixture.author,
                "must not advance main",
            )
            .unwrap_err(),
        fixture
            .store
            .remove_worktree(&fixture.repository_id, &worktree_id, branch, false)
            .unwrap_err(),
    ] {
        assert!(error.to_string().contains("expected immutable branch"));
    }
    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, MAIN_BRANCH)
            .unwrap(),
        initial
    );
    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, branch)
            .unwrap(),
        initial
    );

    linked.set_head_detached(initial.oid().unwrap()).unwrap();
    let detached = fixture
        .store
        .commit_worktree_files(
            &fixture.repository_id,
            &worktree_id,
            branch,
            &initial,
            &FileMap::new(),
            &file_map([("state.json", "detached")]),
            &fixture.author,
            "must not commit detached",
        )
        .unwrap_err();
    assert!(detached.to_string().contains("detached HEAD"));

    linked.set_head(&format!("refs/heads/{branch}")).unwrap();
    fixture
        .store
        .remove_worktree(&fixture.repository_id, &worktree_id, branch, false)
        .unwrap();
}

#[test]
fn worktree_creation_never_overwrites_an_existing_path() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "agent", &initial)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-existing").unwrap();
    let path = fixture
        .store
        .storage()
        .authored_worktree_dir(fixture.repository_id.as_str(), worktree_id.as_str());
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("user-data"), b"keep").unwrap();
    let error = fixture
        .store
        .create_worktree(&fixture.repository_id, "agent", &worktree_id)
        .unwrap_err();
    assert!(matches!(error, AuthoredStateError::UnsafePath(_)));
    assert_eq!(fs::read(path.join("user-data")).unwrap(), b"keep");
}

#[test]
fn worktree_creation_recovers_a_registered_checkout_missing_from_disk() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "recover-missing", &initial)
        .unwrap();
    let id = WorktreeId::parse("w-recover-missing").unwrap();
    let first = fixture
        .store
        .create_worktree(&fixture.repository_id, "recover-missing", &id)
        .unwrap();
    fs::remove_dir_all(&first.path).unwrap();

    let recovered = fixture
        .store
        .create_worktree(&fixture.repository_id, "recover-missing", &id)
        .unwrap();
    assert_eq!(recovered.branch, "recover-missing");
    assert_eq!(recovered.head, initial);
    assert!(recovered.path.join(".git").is_file());
    assert_eq!(
        fixture
            .store
            .list_worktrees(&fixture.repository_id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn worktree_creation_recovers_only_a_safe_unregistered_partial_checkout() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "recover-partial", &initial)
        .unwrap();
    let committed = fixture.commit(
        "recover-partial",
        &initial,
        "score.luma",
        "expected",
        "seed recovery branch",
    );
    let id = WorktreeId::parse("w-recover-partial").unwrap();
    let path = fixture
        .store
        .storage()
        .authored_worktree_dir(fixture.repository_id.as_str(), id.as_str());
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("score.luma"), b"expected").unwrap();

    let recovered = fixture
        .store
        .create_worktree(&fixture.repository_id, "recover-partial", &id)
        .unwrap();
    assert_eq!(recovered.head, committed.id);
    assert_eq!(
        fs::read(recovered.path.join("score.luma")).unwrap(),
        b"expected"
    );
    assert!(recovered.path.join(".git").is_file());
}

#[test]
fn worktree_removal_prunes_registered_metadata_when_checkout_is_missing() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "remove-missing", &initial)
        .unwrap();
    let id = WorktreeId::parse("w-remove-missing").unwrap();
    let worktree = fixture
        .store
        .create_worktree(&fixture.repository_id, "remove-missing", &id)
        .unwrap();
    fs::remove_dir_all(&worktree.path).unwrap();

    fixture
        .store
        .remove_worktree(&fixture.repository_id, &id, "remove-missing", false)
        .unwrap();
    assert!(fixture
        .store
        .list_worktrees(&fixture.repository_id)
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, "remove-missing")
            .unwrap(),
        initial
    );
}

#[cfg(unix)]
#[test]
fn symlinks_are_rejected_in_repository_and_worktree_projections() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "agent", &initial)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-symlink").unwrap();
    let worktree = fixture
        .store
        .create_worktree(&fixture.repository_id, "agent", &worktree_id)
        .unwrap();
    let outside = fixture._directory.path().join("outside");
    fs::write(&outside, b"secret").unwrap();
    symlink(&outside, worktree.path.join("state.json")).unwrap();
    let error = fixture
        .store
        .read_worktree_files(&fixture.repository_id, &worktree_id, "agent")
        .unwrap_err();
    assert!(matches!(error, AuthoredStateError::UnsafePath(_)));
    let removal_error = fixture
        .store
        .remove_worktree(&fixture.repository_id, &worktree_id, "agent", false)
        .unwrap_err();
    assert!(matches!(removal_error, AuthoredStateError::UnsafePath(_)));
    assert!(worktree.path.join("state.json").is_symlink());
    fs::remove_file(worktree.path.join("state.json")).unwrap();
    fixture
        .store
        .remove_worktree(&fixture.repository_id, &worktree_id, "agent", false)
        .unwrap();

    let malicious_id = AuthoredRepositoryId::derive(
        &AuthoredRepositoryDescriptor::pattern_graph("user-a", "malicious", "implementation")
            .unwrap(),
    );
    fs::create_dir_all(fixture.store.storage().authored_repositories_dir()).unwrap();
    let repository_path = fixture
        .store
        .storage()
        .authored_repository_dir(malicious_id.as_str());
    symlink(fixture._directory.path(), &repository_path).unwrap();
    let error = fixture.store.ensure_repository(&malicious_id).unwrap_err();
    assert!(matches!(error, AuthoredStateError::UnsafePath(_)));
}

#[cfg(unix)]
#[test]
fn repository_create_and_open_reject_symlinked_authored_state_ancestors() {
    use std::os::unix::fs::symlink;

    let create_fixture = Fixture::new();
    let storage_root = create_fixture.store.storage().path();
    fs::create_dir_all(storage_root).unwrap();
    let outside = create_fixture._directory.path().join("outside-create");
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, storage_root.join("authored-state")).unwrap();

    let create_error = create_fixture
        .store
        .ensure_repository(&create_fixture.repository_id)
        .unwrap_err();
    assert!(matches!(create_error, AuthoredStateError::UnsafePath(_)));
    assert!(fs::read_dir(&outside).unwrap().next().is_none());

    let open_fixture = Fixture::new();
    open_fixture.init();
    let repositories = open_fixture.store.storage().authored_repositories_dir();
    let parked = open_fixture
        .store
        .storage()
        .authored_state_dir()
        .join("parked-repositories");
    fs::rename(&repositories, &parked).unwrap();
    let outside = open_fixture._directory.path().join("outside-open");
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, &repositories).unwrap();

    let open_error = open_fixture
        .store
        .main_head(&open_fixture.repository_id)
        .unwrap_err();
    assert!(matches!(open_error, AuthoredStateError::UnsafePath(_)));
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn worktree_create_read_and_remove_reject_symlinked_ancestor() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "agent", &initial)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-ancestor").unwrap();
    fixture
        .store
        .create_worktree(&fixture.repository_id, "agent", &worktree_id)
        .unwrap();

    let worktrees = fixture.store.storage().authored_worktrees_dir();
    let parked = fixture
        .store
        .storage()
        .authored_state_dir()
        .join("parked-worktrees");
    fs::rename(&worktrees, &parked).unwrap();
    let outside = fixture._directory.path().join("outside-worktrees");
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, &worktrees).unwrap();

    let create_error = fixture
        .store
        .create_worktree(
            &fixture.repository_id,
            "agent",
            &WorktreeId::parse("w-second").unwrap(),
        )
        .unwrap_err();
    let read_error = fixture
        .store
        .read_worktree_files(&fixture.repository_id, &worktree_id, "agent")
        .unwrap_err();
    let remove_error = fixture
        .store
        .remove_worktree(&fixture.repository_id, &worktree_id, "agent", false)
        .unwrap_err();
    for error in [create_error, read_error, remove_error] {
        assert!(matches!(error, AuthoredStateError::UnsafePath(_)));
    }
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn externally_registered_symlink_alias_cannot_impersonate_bounded_worktree_path() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "agent", &initial)
        .unwrap();
    let worktree_id = WorktreeId::parse("w-external-alias").unwrap();
    let expected = fixture
        .store
        .storage()
        .authored_worktree_dir(fixture.repository_id.as_str(), worktree_id.as_str());
    fs::create_dir_all(expected.parent().unwrap()).unwrap();
    let external = fixture._directory.path().join("external-registration");

    let repo = fixture
        .store
        .open_repository(&fixture.repository_id)
        .unwrap();
    let reference = repo.find_reference("refs/heads/agent").unwrap();
    let mut options = WorktreeAddOptions::new();
    options.reference(Some(&reference));
    repo.worktree(worktree_id.as_str(), &external, Some(&options))
        .unwrap();
    drop(reference);
    drop(repo);
    fs::rename(&external, &expected).unwrap();
    symlink(&expected, &external).unwrap();
    assert_eq!(
        external.canonicalize().unwrap(),
        expected.canonicalize().unwrap()
    );

    let create_error = fixture
        .store
        .create_worktree(&fixture.repository_id, "agent", &worktree_id)
        .unwrap_err();
    let read_error = fixture
        .store
        .read_worktree_files(&fixture.repository_id, &worktree_id, "agent")
        .unwrap_err();
    let remove_error = fixture
        .store
        .remove_worktree(&fixture.repository_id, &worktree_id, "agent", false)
        .unwrap_err();
    for error in [create_error, read_error, remove_error] {
        assert!(matches!(error, AuthoredStateError::UnsafePath(_)));
    }
    assert!(external.is_symlink());
    assert!(expected.is_dir());
}

#[test]
fn locked_worktree_requires_explicit_force_to_remove() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "locked", &initial)
        .unwrap();
    let id = WorktreeId::parse("w-locked").unwrap();
    fixture
        .store
        .create_worktree(&fixture.repository_id, "locked", &id)
        .unwrap();
    let repo = fixture
        .store
        .open_repository(&fixture.repository_id)
        .unwrap();
    repo.find_worktree(id.as_str())
        .unwrap()
        .lock(Some("test"))
        .unwrap();
    assert!(fixture
        .store
        .remove_worktree(&fixture.repository_id, &id, "locked", false)
        .is_err());
    fixture
        .store
        .remove_worktree(&fixture.repository_id, &id, "locked", true)
        .unwrap();
}

#[test]
fn dirty_and_untracked_worktrees_are_never_removed() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;

    fixture
        .store
        .create_branch(&fixture.repository_id, "dirty", &initial)
        .unwrap();
    let dirty_id = WorktreeId::parse("w-dirty").unwrap();
    let dirty = fixture
        .store
        .create_worktree(&fixture.repository_id, "dirty", &dirty_id)
        .unwrap();
    let committed = fixture
        .store
        .commit_worktree_files(
            &fixture.repository_id,
            &dirty_id,
            "dirty",
            &initial,
            &FileMap::new(),
            &file_map([("score.luma", "committed")]),
            &fixture.author,
            "add score",
        )
        .unwrap();
    fs::write(dirty.path.join("score.luma"), b"uncommitted").unwrap();
    let error = fixture
        .store
        .remove_worktree(&fixture.repository_id, &dirty_id, "dirty", false)
        .unwrap_err();
    assert!(error.to_string().contains("uncommitted or untracked"));
    assert_eq!(
        fs::read(dirty.path.join("score.luma")).unwrap(),
        b"uncommitted"
    );
    assert_eq!(
        fixture
            .store
            .branch_head(&fixture.repository_id, "dirty")
            .unwrap(),
        committed.id
    );

    fixture
        .store
        .create_branch(&fixture.repository_id, "untracked", &initial)
        .unwrap();
    let untracked_id = WorktreeId::parse("w-untracked").unwrap();
    let untracked = fixture
        .store
        .create_worktree(&fixture.repository_id, "untracked", &untracked_id)
        .unwrap();
    fs::write(untracked.path.join("notes.txt"), b"do not lose").unwrap();
    let error = fixture
        .store
        .remove_worktree(&fixture.repository_id, &untracked_id, "untracked", false)
        .unwrap_err();
    assert!(error.to_string().contains("uncommitted or untracked"));
    assert_eq!(
        fs::read(untracked.path.join("notes.txt")).unwrap(),
        b"do not lose"
    );
}

#[test]
fn oversized_worktree_files_are_rejected_from_metadata_before_reading() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    fixture
        .store
        .create_branch(&fixture.repository_id, "oversized", &initial)
        .unwrap();
    let id = WorktreeId::parse("w-oversized").unwrap();
    let worktree = fixture
        .store
        .create_worktree(&fixture.repository_id, "oversized", &id)
        .unwrap();
    let sparse = fs::File::create(worktree.path.join("score.luma")).unwrap();
    sparse.set_len((MAX_FILE_BYTES + 1) as u64).unwrap();

    let error = fixture
        .store
        .read_worktree_files(&fixture.repository_id, &id, "oversized")
        .unwrap_err();
    assert!(error.to_string().contains("exceeds"));
}

#[test]
fn oversized_git_blobs_are_rejected_from_the_object_header_before_copying() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let repo = fixture
        .store
        .open_repository(&fixture.repository_id)
        .unwrap();
    let blob = repo.blob(&vec![0; MAX_FILE_BYTES + 1]).unwrap();
    let mut builder = repo.treebuilder(None).unwrap();
    builder.insert("score.luma", blob, 0o100644).unwrap();
    let tree_id = builder.write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parent = repo.find_commit(initial.oid().unwrap()).unwrap();
    let signature = fixture.author.signature().unwrap();
    let commit = repo
        .commit(
            None,
            &signature,
            &signature,
            "malicious oversized blob",
            &tree,
            &[&parent],
        )
        .unwrap();

    let error = fixture
        .store
        .read_commit(&fixture.repository_id, &CommitId::from_oid(commit))
        .unwrap_err();
    assert!(error.to_string().contains("exceeds"));
}

#[test]
fn merge_commit_preserves_parent_order_and_restore_creates_new_history() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let base = fixture.commit(MAIN_BRANCH, &initial, "state.json", "base", "base");
    fixture
        .store
        .create_branch(&fixture.repository_id, "feature", &base.id)
        .unwrap();
    let ours = fixture.commit(MAIN_BRANCH, &base.id, "state.json", "ours", "ours");
    let theirs = fixture.commit("feature", &base.id, "feature.json", "theirs", "theirs");
    assert_eq!(
        fixture
            .store
            .merge_base(&fixture.repository_id, &ours.id, &theirs.id)
            .unwrap(),
        base.id
    );

    let merged_files = file_map([("feature.json", "theirs"), ("state.json", "ours")]);
    let merged = fixture
        .store
        .create_merge_commit(
            &fixture.repository_id,
            MAIN_BRANCH,
            &ours.id,
            &theirs.id,
            &merged_files,
            &fixture.author,
            "merge feature",
        )
        .unwrap();
    assert_eq!(merged.parents, vec![ours.id.clone(), theirs.id.clone()]);
    assert_eq!(
        fixture
            .store
            .read_commit(&fixture.repository_id, &merged.id)
            .unwrap()
            .1,
        merged_files
    );

    let restored = fixture
        .store
        .restore_as_commit(
            &fixture.repository_id,
            MAIN_BRANCH,
            &merged.id,
            &base.id,
            &fixture.author,
            "restore base",
        )
        .unwrap();
    assert_ne!(restored.id, base.id);
    assert_eq!(restored.parents, vec![merged.id]);
    assert_eq!(
        fixture
            .store
            .read_commit(&fixture.repository_id, &restored.id)
            .unwrap()
            .1,
        file_map([("state.json", "base")])
    );
}

#[test]
fn criss_cross_merge_bases_are_complete_and_singular_resolution_fails_closed() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let base = fixture.commit(MAIN_BRANCH, &initial, "base", "base", "base");
    fixture
        .store
        .create_branch(&fixture.repository_id, "left", &base.id)
        .unwrap();
    fixture
        .store
        .create_branch(&fixture.repository_id, "right", &base.id)
        .unwrap();
    let left = fixture.commit("left", &base.id, "left", "one", "left");
    let right = fixture.commit("right", &base.id, "right", "one", "right");
    let both = file_map([("left", "one"), ("right", "one")]);
    let left_merge = fixture
        .store
        .create_merge_commit(
            &fixture.repository_id,
            "left",
            &left.id,
            &right.id,
            &both,
            &fixture.author,
            "left merge",
        )
        .unwrap();
    let right_merge = fixture
        .store
        .create_merge_commit(
            &fixture.repository_id,
            "right",
            &right.id,
            &left.id,
            &both,
            &fixture.author,
            "right merge",
        )
        .unwrap();

    let bases = fixture
        .store
        .merge_bases(&fixture.repository_id, &left_merge.id, &right_merge.id)
        .unwrap();
    let mut expected = vec![left.id, right.id];
    expected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    assert_eq!(bases, expected);
    let error = fixture
        .store
        .merge_base(&fixture.repository_id, &left_merge.id, &right_merge.id)
        .unwrap_err();
    assert!(error.to_string().contains("multiple best merge bases"));
}

#[test]
fn malicious_symlink_git_tree_is_never_projected_as_a_regular_file() {
    let fixture = Fixture::new();
    let initial = fixture.init().main_head;
    let repo = fixture
        .store
        .open_repository(&fixture.repository_id)
        .unwrap();
    let symlink_blob = repo.blob(b"../../outside").unwrap();
    let mut builder = repo.treebuilder(None).unwrap();
    builder
        .insert("state.json", symlink_blob, 0o120000)
        .unwrap();
    let tree_id = builder.write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parent = repo.find_commit(initial.oid().unwrap()).unwrap();
    let signature = fixture.author.signature().unwrap();
    let commit_id = repo
        .commit(None, &signature, &signature, "malicious", &tree, &[&parent])
        .unwrap();
    let error = fixture
        .store
        .read_commit(&fixture.repository_id, &CommitId::from_oid(commit_id))
        .unwrap_err();
    assert!(matches!(error, AuthoredStateError::UnsafePath(_)));
}
