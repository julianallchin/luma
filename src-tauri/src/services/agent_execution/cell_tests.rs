//! Full-pipeline acceptance tests (design §22): a real database, a real
//! workspace, a real Python kernel, driven through the same
//! [`run_python_cell_inner`] the dispatch handler and the harness call.
//!
//! Nothing is mocked below the command boundary — the providers read the
//! synthetic library, the manifest is written to disk, the worker loads it, and
//! the numbers the agent sees are the numbers SQLite holds. Machines without the
//! managed venv skip, exactly like `kernel_tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::agent_execution::graph_runs::GraphRunStore;
use crate::agent_execution::sandbox;
use crate::agent_execution::workspace::{PythonWorkspaceService, WorkerEnv};
use crate::eval::graph_run::{evaluate_graph, EvaluateOptions};
use crate::models::agent_execution::{PythonCellResult, PythonScopeInput};
use crate::models::agent_threads::{
    AppendAgentThreadMessagesInput, CreateAgentThreadInput, NewAgentThreadMessage,
};
use crate::models::authored_state::{CreateAuthoredWorkspaceInput, PrepareAuthoredTurnInput};
use crate::models::node_graph::{
    Edge, Graph, GraphContext, NodeInstance, PatternArgDef, PatternArgType,
};
use crate::services::agent_execution::{cancel_python_cell_inner, run_python_cell_inner};
use crate::services::authored_documents::AuthoredDocuments;
use crate::storage::StorageRoot;

const TRACK_ID: &str = "trk-cell";
const TRACK_HASH: &str = "hashcell";
const SCORE_ID: &str = "score-cell";
const ANALYSIS_SCORE_ID: &str = "analysis-score-cell";
const PATTERN_ID: &str = "pattern-cell";
const SAMPLE_RATE: u32 = 48_000;
/// Absolute seconds; the values the assertions below are computed from.
const KICKS: [f64; 3] = [0.5, 1.5, 2.5];
const BEATS: [f64; 4] = [0.5, 1.0, 1.5, 2.0];

fn venv_python() -> Option<PathBuf> {
    let path = dirs::cache_dir()?
        .join("com.luma.luma")
        .join("python-env")
        .join("bin")
        .join("python3");
    path.exists().then_some(path)
}

fn worker_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("python")
        .join("luma_exec")
        .join("worker.py")
}

/// The bundled QLC+ definitions, so a venue fixture expands to real heads.
fn repo_fixtures_root() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("resources/fixtures");
    std::fs::read_dir(&dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .max()
        })
        .unwrap_or(dir)
}

async fn test_pool(dir: &Path) -> SqlitePool {
    let db_path = dir.join("luma-test.db");
    let migrate_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .expect("migrate pool");
    sqlx::migrate!("./migrations")
        .run(&migrate_pool)
        .await
        .expect("migrations");
    migrate_pool.close().await;

    SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .expect("pool")
}

struct Fixture {
    _dir: tempfile::TempDir,
    pool: SqlitePool,
    storage: StorageRoot,
    authored: AuthoredDocuments,
    resource_root: PathBuf,
    service: PythonWorkspaceService,
    graph_runs: GraphRunStore,
    venue_id: String,
}

impl Fixture {
    /// `None` on a machine that has never run Luma (no managed venv).
    async fn new(name: &str) -> Option<Self> {
        let Some(python_bin) = venv_python() else {
            eprintln!("[skip] {name}: no managed python env at ~/Library/Caches/com.luma.luma");
            return None;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(dir.path()).await;
        let storage = StorageRoot::from_path(dir.path().join("config"));
        let env = WorkerEnv::new(
            python_bin,
            worker_script(),
            Arc::new(sandbox::default_launcher),
        );
        let service = PythonWorkspaceService::with_env(storage.agent_workspaces_dir(), env);
        let authored = AuthoredDocuments::new(storage.clone());

        let f = Self {
            _dir: dir,
            pool,
            storage,
            authored,
            resource_root: repo_fixtures_root(),
            service,
            graph_runs: GraphRunStore::new(),
            // Unique per test: `services::groups` keeps a process-wide cache.
            venue_id: format!("ven-{}", uuid::Uuid::new_v4()),
        };
        f.seed().await;
        Some(f)
    }

    async fn seed(&self) {
        std::fs::create_dir_all(self.storage.tracks_dir()).unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, track_hash, title, artist, duration_seconds, file_path)
             VALUES (?, ?, 'Hex', 'Surgeon', 8.0, ?)",
        )
        .bind(TRACK_ID)
        .bind(TRACK_HASH)
        .bind(
            self.storage
                .tracks_dir()
                .join("hex.wav")
                .to_string_lossy()
                .to_string(),
        )
        .execute(&self.pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO track_beats (track_id, beats_json, downbeats_json, bpm, beats_per_bar)
             VALUES (?, ?, '[0.5,2.5]', 120.0, 4)",
        )
        .bind(TRACK_ID)
        .bind(json!(BEATS).to_string())
        .execute(&self.pool)
        .await
        .unwrap();

        sqlx::query("INSERT INTO track_drum_onsets (track_id, onsets_json) VALUES (?, ?)")
            .bind(TRACK_ID)
            .bind(json!({ "kick": KICKS, "snare": [1.0], "hat": [] }).to_string())
            .execute(&self.pool)
            .await
            .unwrap();

        // A 4-second mono mix: a 1 Hz sine at 0.75 peak, so `max(|x|)` is known.
        let samples: Vec<f32> = (0..SAMPLE_RATE * 4)
            .map(|i| (i as f32 / SAMPLE_RATE as f32 * std::f32::consts::TAU).sin() * 0.75)
            .collect();
        crate::audio::cache::write_pcm_file(
            &self.storage.mix_pcm_path(TRACK_HASH),
            &samples,
            SAMPLE_RATE,
            1,
        )
        .unwrap();

        sqlx::query("INSERT INTO venues (id, name) VALUES (?, 'Basement')")
            .bind(&self.venue_id)
            .execute(&self.pool)
            .await
            .unwrap();
        for (i, id) in ["fix-a", "fix-b"].iter().enumerate() {
            sqlx::query(
                "INSERT INTO fixtures (id, venue_id, universe, address, num_channels,
                    manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z)
                 VALUES (?, ?, 1, ?, 8, 'Chauvet', 'SlimPAR', '8-Channel',
                    'Chauvet/SlimPAR.qxf', ?, ?, 0.0, 2.0)",
            )
            .bind(id)
            .bind(&self.venue_id)
            .bind(1 + i as i64 * 8)
            .bind(format!("PAR {}", i + 1))
            .bind(i as f64)
            .execute(&self.pool)
            .await
            .unwrap();
        }
    }

    async fn thread(&self, agent_kind: &str) -> String {
        let (subject_kind, subject_id, implementation_id, score_id) =
            if agent_kind == "track_copilot" {
                // A track thread's durable venue and score are one indivisible
                // guest scope. Mutation authorization is exercised separately;
                // these tests care only about analysis and namespace behavior.
                sqlx::query(
                    "INSERT OR IGNORE INTO scores (id, uid, track_id, venue_id, name)
                 VALUES (?, NULL, ?, ?, 'Analysis')",
                )
                .bind(ANALYSIS_SCORE_ID)
                .bind(TRACK_ID)
                .bind(&self.venue_id)
                .execute(&self.pool)
                .await
                .unwrap();
                ("track", TRACK_ID, None, Some(ANALYSIS_SCORE_ID.to_string()))
            } else {
                sqlx::query(
                    "INSERT OR IGNORE INTO patterns (id, name, description, is_verified)
                 VALUES (?, 'Blank canvas', 'empty test graph', 1)",
                )
                .bind(PATTERN_ID)
                .execute(&self.pool)
                .await
                .unwrap();
                sqlx::query(
                    "INSERT OR IGNORE INTO implementations (id, pattern_id, graph_json)
                 VALUES ('implementation-cell', ?, '{\"nodes\":[],\"edges\":[],\"args\":[]}')",
                )
                .bind(PATTERN_ID)
                .execute(&self.pool)
                .await
                .unwrap();
                (
                    "pattern",
                    PATTERN_ID,
                    Some("implementation-cell".to_string()),
                    None,
                )
            };
        crate::database::local::auth::arm_write_admission(&self.pool, None)
            .await
            .unwrap();
        crate::database::local::agent_threads::create_thread(
            &self.pool,
            CreateAgentThreadInput {
                agent_kind: agent_kind.to_string(),
                subject_kind: Some(subject_kind.into()),
                subject_id: Some(subject_id.into()),
                implementation_id,
                venue_id: Some(self.venue_id.clone()),
                score_id,
                ..Default::default()
            },
            None,
        )
        .await
        .expect("create thread")
        .id
    }

    async fn editable_thread(&self) -> String {
        sqlx::query(
            "INSERT OR IGNORE INTO patterns (id, name, description, is_verified)
             VALUES (?, 'Blank canvas', 'empty test graph', 1)",
        )
        .bind(PATTERN_ID)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO implementations (id, pattern_id, graph_json)
             VALUES ('implementation-cell', ?, '{\"nodes\":[],\"edges\":[],\"args\":[]}')",
        )
        .bind(PATTERN_ID)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name)
             VALUES (?, 'owner', ?, ?, 'Main')",
        )
        .bind(SCORE_ID)
        .bind(TRACK_ID)
        .bind(&self.venue_id)
        .execute(&self.pool)
        .await
        .unwrap();

        for table in ["venues", "fixtures", "track_beats", "track_drum_onsets"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE {table} SET uid = 'owner'"
            )))
            .execute(&self.pool)
            .await
            .unwrap();
        }
        crate::database::local::auth::arm_write_admission(&self.pool, Some("owner"))
            .await
            .unwrap();

        crate::database::local::agent_threads::create_thread(
            &self.pool,
            CreateAgentThreadInput {
                agent_kind: "track_copilot".into(),
                subject_kind: Some("track".into()),
                subject_id: Some(TRACK_ID.into()),
                venue_id: Some(self.venue_id.clone()),
                score_id: Some(SCORE_ID.into()),
                ..Default::default()
            },
            Some("owner"),
        )
        .await
        .unwrap()
        .id
    }

    fn scope(&self) -> PythonScopeInput {
        PythonScopeInput {
            track_id: Some(TRACK_ID.into()),
            venue_id: Some(self.venue_id.clone()),
            window: Some((0.0, 4.0)),
            ..Default::default()
        }
    }

    async fn run(&self, thread_id: &str, code: &str) -> PythonCellResult {
        self.run_scoped(thread_id, code, self.scope()).await
    }

    async fn run_scoped(
        &self,
        thread_id: &str,
        code: &str,
        scope: PythonScopeInput,
    ) -> PythonCellResult {
        run_python_cell_inner(
            &self.pool,
            &self.storage,
            &self.resource_root,
            &self.service,
            &self.graph_runs,
            &self.authored,
            thread_id.to_string(),
            code.to_string(),
            scope,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("run_python_cell")
    }

    async fn run_as_owner(
        &self,
        thread_id: &str,
        turn_message_id: &str,
        code: &str,
    ) -> PythonCellResult {
        let mut scope = self.scope();
        scope.score_id = Some(SCORE_ID.into());
        run_python_cell_inner(
            &self.pool,
            &self.storage,
            &self.resource_root,
            &self.service,
            &self.graph_runs,
            &self.authored,
            thread_id.to_string(),
            code.to_string(),
            scope,
            Some(turn_message_id.to_string()),
            Some("owner".into()),
            None,
            None,
        )
        .await
        .expect("run authorized Python cell")
    }

    async fn run_as_owner_in_workspace(
        &self,
        thread_id: &str,
        workspace_id: &str,
        turn_message_id: &str,
        code: &str,
    ) -> PythonCellResult {
        let mut scope = self.scope();
        scope.score_id = Some(SCORE_ID.into());
        run_python_cell_inner(
            &self.pool,
            &self.storage,
            &self.resource_root,
            &self.service,
            &self.graph_runs,
            &self.authored,
            thread_id.to_string(),
            code.to_string(),
            scope,
            Some(turn_message_id.to_string()),
            Some("owner".into()),
            Some(workspace_id.to_string()),
            Some(workspace_id.to_string()),
        )
        .await
        .expect("run authorized workspace Python cell")
    }
}

#[track_caller]
fn expect_ok(result: &PythonCellResult, what: &str) {
    assert_eq!(
        result.status, "ok",
        "{what}\nstdout: {}\nstderr: {}\ntraceback: {:?}\nnotices: {:?}",
        result.stdout, result.stderr, result.traceback, result.notices
    );
}

#[track_caller]
fn repr_f64(result: &PythonCellResult, what: &str) -> f64 {
    expect_ok(result, what);
    result
        .repr
        .as_deref()
        .unwrap_or_else(|| panic!("{what}: no repr"))
        .parse()
        .unwrap_or_else(|e| panic!("{what}: repr {:?} is not a float: {e}", result.repr))
}

// ---------------------------------------------------------------------------
// §22.3-§22.7, §22.11 — the track copilot's loop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_thread_computes_over_its_track_and_keeps_its_namespace() {
    let Some(f) = Fixture::new("a_thread_computes_over_its_track_and_keeps_its_namespace").await
    else {
        return;
    };
    let thread = f.thread("track_copilot").await;

    // §22.6 — precomputed drum onsets, straight out of the binding.
    let out = f
        .run(
            &thread,
            "kicks = luma.features.drum_onsets[\"kick\"].values\nfloat(kicks.mean())",
        )
        .await;
    let expected = KICKS.iter().sum::<f64>() / KICKS.len() as f64;
    assert!(
        (repr_f64(&out, "kick mean") - expected).abs() < 1e-4,
        "kick mean: {:?}",
        out.repr
    );

    // §22.7 — the audio mix itself, independent of any precomputed feature.
    let out = f
        .run(
            &thread,
            "import numpy as np\nfloat(np.abs(luma.audio.mix.values).max())",
        )
        .await;
    assert!(
        (repr_f64(&out, "mix peak") - 0.75).abs() < 0.02,
        "mix peak: {:?}",
        out.repr
    );

    // §22.3 — a helper defined in one cell is callable in the next.
    expect_ok(
        &f.run(
            &thread,
            "def gaps(times):\n    return [round(float(b - a), 3) for a, b in zip(times, times[1:])]\n",
        )
        .await,
        "define helper",
    );
    let out = f.run(&thread, "gaps(luma.features.beats.values)").await;
    expect_ok(&out, "use helper");
    assert_eq!(out.repr.as_deref(), Some("[0.5, 0.5, 0.5]"));

    // §22.5 — the analysis changes underneath the conversation. The next cell
    // sees the new beats; the helper and `kicks` survive.
    sqlx::query("UPDATE track_beats SET beats_json = ? WHERE track_id = ?")
        .bind(json!([0.0, 2.0, 4.0]).to_string())
        .bind(TRACK_ID)
        .execute(&f.pool)
        .await
        .unwrap();
    let out = f
        .run(
            &thread,
            "(gaps(luma.features.beats.values), len(kicks), float(luma.features.beats.values[-1]))",
        )
        .await;
    expect_ok(&out, "refreshed binding");
    assert_eq!(out.repr.as_deref(), Some("([2.0, 2.0], 3, 4.0)"));

    // §22.11 — a figure comes back with real pixel dimensions and bytes.
    let out = f
        .run(
            &thread,
            "import matplotlib.pyplot as plt\n\
             plt.figure(figsize=(4, 2), dpi=100)\n\
             plt.plot(luma.features.beats.values)\n",
        )
        .await;
    expect_ok(&out, "figure");
    assert_eq!(out.figures.len(), 1, "figures: {}", out.figures.len());
    let figure = &out.figures[0];
    assert_eq!((figure.width, figure.height), (400, 200));
    assert!(figure.artifact_rel.starts_with("outputs/"));
    assert!(figure.base64_png.len() > 1000, "base64 png looks empty");
    let png = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &figure.base64_png,
    )
    .expect("base64");
    assert!(png.starts_with(b"\x89PNG"), "figure is not a PNG");

    // §22.4 — a second thread is a second namespace.
    let other = f.thread("track_copilot").await;
    let out = f.run(&other, "kicks").await;
    assert_eq!(out.status, "error", "second thread saw {:?}", out.repr);
    assert!(out.traceback.unwrap_or_default().contains("NameError"));

    f.service.shutdown_all();
}

#[tokio::test]
async fn python_track_edit_applies_through_the_real_worker_and_transaction() {
    let Some(f) =
        Fixture::new("python_track_edit_applies_through_the_real_worker_and_transaction").await
    else {
        return;
    };
    let thread = f.editable_thread().await;
    let turn_message_id = "turn-apply-one-clip";
    crate::database::local::agent_threads::append_messages(
        &f.pool,
        &thread,
        AppendAgentThreadMessagesInput {
            operation_id: "test-turn-apply-one-clip".into(),
            expected_head_message_id: None,
            messages: vec![NewAgentThreadMessage {
                id: Some(turn_message_id.into()),
                role: "user".into(),
                parts: json!([{ "type": "text", "text": "add one clip" }]),
            }],
        },
        Some("owner"),
    )
    .await
    .unwrap();
    let code = "edit = luma.track.edit()\n\
                added = edit.add_clip('Blank canvas', seconds=(1.0, 2.0), z=0)\n\
                checked = edit.check()\n\
                rendered = edit.window(seconds=(1.0, 2.0)).output.tensor\n\
                payload = edit._plan()\n\
                applied = edit.apply()\n\
                (bool(checked), rendered.shape, applied.added, len(applied.clips), added.id.startswith('new:'))";
    let out = f.run_as_owner(&thread, turn_message_id, code).await;
    expect_ok(&out, "apply track candidate");
    assert_eq!(out.repr.as_deref(), Some("(True, (2, 32, 3), 1, 1, True)"));

    // A regenerated remote tool call has a new provider ID, but the durable
    // turn and exact host request are unchanged. Replaying the captured plan
    // must resolve its operation before rejecting its now-stale base revision.
    let replayed = f
        .run_as_owner(
            &thread,
            turn_message_id,
            "replayed = _luma_host_call('track.apply', payload)\n\
             (replayed['added'], len(replayed['clips']))",
        )
        .await;
    expect_ok(&replayed, "replay committed track candidate");
    assert_eq!(replayed.repr.as_deref(), Some("(1, 1)"));

    // The durable turn is only a namespace. A different plan receives its own
    // operation identity and therefore reaches ordinary stale-base validation.
    let changed = f
        .run_as_owner(
            &thread,
            turn_message_id,
            "import copy\n\
             changed = copy.deepcopy(payload)\n\
             changed['candidate'][0]['endTime'] = 2.5\n\
             _luma_host_call('track.apply', changed)",
        )
        .await;
    assert_eq!(changed.status, "error", "{changed:?}");
    assert!(
        changed
            .traceback
            .as_deref()
            .unwrap_or_default()
            .contains("track changed while this edit was open"),
        "{:?}",
        changed.traceback
    );

    // A genuinely different plan in the same turn has a different content
    // identity and commits normally against the refreshed current revision.
    let independent = f
        .run_as_owner(
            &thread,
            turn_message_id,
            "edit2 = luma.track.edit()\n\
             edit2.add_clip('Blank canvas', seconds=(3.0, 4.0), z=0)\n\
             applied2 = edit2.apply()\n\
             (applied2.added, len(applied2.clips))",
        )
        .await;
    expect_ok(&independent, "apply independent plan in same turn");
    assert_eq!(independent.repr.as_deref(), Some("(1, 2)"));

    // A very late retry still correlates with its original commit and result,
    // while returning the newer authoritative main document.
    let late_replay = f
        .run_as_owner(
            &thread,
            turn_message_id,
            "late = _luma_host_call('track.apply', payload)\n\
             (late['added'], len(late['clips']), late['appliedToCurrentProjection'])",
        )
        .await;
    expect_ok(&late_replay, "replay original plan after newer main");
    assert_eq!(late_replay.repr.as_deref(), Some("(1, 2, False)"));

    let mut access = crate::database::local::venue_access::VenueAccess::<
        crate::database::local::venue_access::Read,
    >::read(
        &f.pool,
        crate::database::local::venue_access::VenueResource::Score(SCORE_ID),
    )
    .await
    .unwrap();
    let rows = crate::database::local::scores::list_track_scores_for_score(&mut access, SCORE_ID)
        .await
        .unwrap();
    drop(access);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.pattern_id == PATTERN_ID));
    assert!(rows.iter().all(|row| !row.id.starts_with("new:")));

    // The next manifest reflects the commit while the ordinary Python
    // namespace survives in the same durable thread.
    let out = f
        .run_as_owner(
            &thread,
            turn_message_id,
            "(len(luma.track.clips), applied.added)",
        )
        .await;
    expect_ok(&out, "refresh committed track binding");
    assert_eq!(out.repr.as_deref(), Some("(2, 1)"));

    // An apply advances `luma.track` inside its own cell. The host reinstalls
    // the binding only between cells, so without that the rest of the cell
    // would render and edit against a revision the host has already superseded.
    let same_cell = f
        .run_as_owner(
            &thread,
            turn_message_id,
            "edit3 = luma.track.edit()\n\
             edit3.add_clip('Blank canvas', seconds=(5.0, 6.0), z=0)\n\
             applied3 = edit3.apply()\n\
             after = luma.track.edit().window(seconds=(5.0, 6.0)).output.tensor\n\
             (applied3.revision == luma.track.revision, len(luma.track.clips), after.shape)",
        )
        .await;
    expect_ok(
        &same_cell,
        "render against the revision the same cell applied",
    );
    assert_eq!(same_cell.repr.as_deref(), Some("(True, 3, (2, 32, 3))"));

    f.service.shutdown_all();
}

#[tokio::test]
async fn python_track_edit_in_child_workspace_never_mutates_the_live_score() {
    let Some(f) =
        Fixture::new("python_track_edit_in_child_workspace_never_mutates_the_live_score").await
    else {
        return;
    };
    let thread = f.editable_thread().await;
    let turn_message_id = "turn-detached-one-clip";
    crate::database::local::agent_threads::append_messages(
        &f.pool,
        &thread,
        AppendAgentThreadMessagesInput {
            operation_id: "test-turn-detached-one-clip".into(),
            expected_head_message_id: None,
            messages: vec![NewAgentThreadMessage {
                id: Some(turn_message_id.into()),
                role: "user".into(),
                parts: json!([{ "type": "text", "text": "delegate one clip" }]),
            }],
        },
        Some("owner"),
    )
    .await
    .unwrap();
    let current = f
        .authored
        .current_revision(&f.pool, Some("owner"), &thread)
        .await
        .unwrap();
    let workspace = f
        .authored
        .create_workspace(
            &f.pool,
            Some("owner"),
            CreateAuthoredWorkspaceInput {
                thread_id: thread.clone(),
                request_id: "cell-detached-workspace".into(),
                expected_base_revision_id: current.revision_id,
            },
        )
        .await
        .unwrap();

    let out = f
        .run_as_owner_in_workspace(
            &thread,
            &workspace.id,
            turn_message_id,
            "edit = luma.track.edit()\n\
             edit.add_clip('Blank canvas', seconds=(1.0, 2.0), z=0)\n\
             applied = edit.apply()\n\
             (applied.added, len(applied.clips))",
        )
        .await;
    expect_ok(&out, "apply detached track candidate");
    assert_eq!(out.repr.as_deref(), Some("(1, 1)"));

    let live_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_scores WHERE score_id = ?")
            .bind(SCORE_ID)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(live_count, 0);
    let check = f
        .authored
        .check_workspace(&f.pool, Some("owner"), &thread, &workspace.id)
        .await
        .unwrap();
    assert!(!check.changed);
    assert_ne!(check.head_revision_id, workspace.base_revision_id);

    let parent = f
        .run_as_owner(&thread, turn_message_id, "len(luma.track.clips)")
        .await;
    expect_ok(&parent, "read live parent track");
    assert_eq!(parent.repr.as_deref(), Some("0"));
    f.service.shutdown_all();
}

#[tokio::test]
async fn python_execution_rejects_a_thread_owned_by_another_principal() {
    let Some(f) =
        Fixture::new("python_execution_rejects_a_thread_owned_by_another_principal").await
    else {
        return;
    };
    let thread = f.editable_thread().await;
    let mut scope = f.scope();
    scope.score_id = Some(SCORE_ID.into());

    let error = run_python_cell_inner(
        &f.pool,
        &f.storage,
        &f.resource_root,
        &f.service,
        &f.graph_runs,
        &f.authored,
        thread.clone(),
        "1 + 1".into(),
        scope,
        None,
        Some("another-user".into()),
        None,
        None,
    )
    .await
    .unwrap_err();

    assert!(error.contains("not available"), "{error}");
    assert!(error.contains("not found"), "{error}");
    assert!(
        !f.storage.agent_workspaces_dir().join(thread).exists(),
        "an unauthorized request must not create or touch the owner's workspace"
    );
}

#[tokio::test]
async fn editable_python_accepts_only_its_own_durable_turn() {
    let Some(f) = Fixture::new("editable_python_accepts_only_its_own_durable_turn").await else {
        return;
    };
    let thread = f.editable_thread().await;
    f.authored
        .prepare_turn(
            &f.pool,
            Some("owner"),
            PrepareAuthoredTurnInput {
                thread_id: thread.clone(),
                assistant_message_id: "assistant-turn".into(),
                graph: None,
            },
        )
        .await
        .unwrap();
    crate::database::local::agent_threads::append_messages(
        &f.pool,
        &thread,
        AppendAgentThreadMessagesInput {
            operation_id: "test-assistant-turn".into(),
            expected_head_message_id: None,
            messages: vec![NewAgentThreadMessage {
                id: Some("assistant-turn".into()),
                role: "assistant".into(),
                parts: json!([]),
            }],
        },
        Some("owner"),
    )
    .await
    .unwrap();
    let foreign_thread = crate::database::local::agent_threads::create_thread(
        &f.pool,
        CreateAgentThreadInput {
            agent_kind: "track_copilot".into(),
            subject_kind: Some("track".into()),
            subject_id: Some(TRACK_ID.into()),
            venue_id: Some(f.venue_id.clone()),
            score_id: Some(SCORE_ID.into()),
            ..Default::default()
        },
        Some("owner"),
    )
    .await
    .unwrap();
    crate::database::local::agent_threads::append_messages(
        &f.pool,
        &foreign_thread.id,
        AppendAgentThreadMessagesInput {
            operation_id: "test-foreign-user-turn".into(),
            expected_head_message_id: None,
            messages: vec![NewAgentThreadMessage {
                id: Some("foreign-user-turn".into()),
                role: "user".into(),
                parts: json!([]),
            }],
        },
        Some("owner"),
    )
    .await
    .unwrap();
    let mut scope = f.scope();
    scope.score_id = Some(SCORE_ID.into());

    for turn_message_id in ["unpersisted-turn", "foreign-user-turn"] {
        let error = run_python_cell_inner(
            &f.pool,
            &f.storage,
            &f.resource_root,
            &f.service,
            &f.graph_runs,
            &f.authored,
            thread.clone(),
            "1 + 1".into(),
            scope.clone(),
            Some(turn_message_id.into()),
            Some("owner".into()),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("not durable in this agent thread"),
            "{error}"
        );
    }
    let assistant_error = run_python_cell_inner(
        &f.pool,
        &f.storage,
        &f.resource_root,
        &f.service,
        &f.graph_runs,
        &f.authored,
        thread.clone(),
        "1 + 1".into(),
        scope,
        Some("assistant-turn".into()),
        Some("owner".into()),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        assistant_error.contains("must be a durable user or session turn"),
        "{assistant_error}"
    );
    assert!(
        !f.storage.agent_workspaces_dir().join(&thread).exists(),
        "an unbound turn must not create or touch the thread workspace"
    );

    // A client that runs cells without speaking opens a *session* turn, and
    // it is as attributable as a user's: this is what lets the MCP host stop
    // fabricating a user message just to have something to attach to.
    crate::database::local::agent_threads::append_messages(
        &f.pool,
        &thread,
        AppendAgentThreadMessagesInput {
            operation_id: "test-session-turn".into(),
            expected_head_message_id: Some("assistant-turn".into()),
            messages: vec![NewAgentThreadMessage {
                id: Some("session-turn".into()),
                role: "session".into(),
                parts: json!([{ "type": "text", "text": "Session opened on Aurora." }]),
            }],
        },
        Some("owner"),
    )
    .await
    .expect("a session turn appends like any other row — no trigger gates it");
    let mut session_scope = f.scope();
    session_scope.score_id = Some(SCORE_ID.into());
    let outcome = run_python_cell_inner(
        &f.pool,
        &f.storage,
        &f.resource_root,
        &f.service,
        &f.graph_runs,
        &f.authored,
        thread.clone(),
        "1 + 1".into(),
        session_scope,
        Some("session-turn".into()),
        Some("owner".into()),
        None,
        None,
    )
    .await
    .expect("an editable cell attaches to a session turn");
    assert_eq!(outcome.status, "ok", "{outcome:?}");
    assert_eq!(outcome.repr.as_deref(), Some("2"));
}

// ---------------------------------------------------------------------------
// §22.8 — the graph agent correlates a run against onsets in one cell
// ---------------------------------------------------------------------------

fn node(id: &str, type_id: &str, params: &[(&str, Value)]) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        type_id: type_id.into(),
        params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        position_x: None,
        position_y: None,
    }
}

fn edge(from: &str, fp: &str, to: &str, tp: &str) -> Edge {
    Edge {
        id: format!("{from}:{fp}->{to}:{tp}"),
        from_node: from.into(),
        from_port: fp.into(),
        to_node: to.into(),
        to_port: tp.into(),
    }
}

/// A ramp over the span, tapped by one view — the smallest graph that produces
/// a time-varying signal on every primitive.
fn ramp_graph() -> Graph {
    Graph {
        nodes: vec![
            node("s0", "scalar", &[("value", Value::from(0.0))]),
            node("s1", "scalar", &[("value", Value::from(1.0))]),
            node("ramp", "ramp_between", &[]),
            node("view_value", "view_signal", &[]),
        ],
        edges: vec![
            edge("s0", "out", "ramp", "start"),
            edge("s1", "out", "ramp", "end"),
            edge("ramp", "out", "view_value", "in"),
        ],
        args: vec![PatternArgDef {
            id: "selection".into(),
            name: "Selection".into(),
            arg_type: PatternArgType::Selection,
            default_value: json!({ "expression": "all", "spatialReference": "global" }),
        }],
    }
}

#[tokio::test]
async fn the_graph_agent_reads_its_own_run_next_to_the_onsets() {
    let Some(f) = Fixture::new("the_graph_agent_reads_its_own_run_next_to_the_onsets").await else {
        return;
    };
    let thread = f.thread("pattern_graph").await;
    let graph = ramp_graph();
    let span = (0.0f32, 4.0f32);

    let evaluation = evaluate_graph(
        &f.pool,
        &f.storage,
        &f.resource_root,
        &crate::audio::FftService::new(),
        &graph,
        &GraphContext {
            track_id: TRACK_ID.into(),
            venue_id: f.venue_id.clone(),
            start_time: span.0,
            end_time: span.1,
            arg_values: None,
            beat_grid: None,
            instance_seed: None,
        },
        EvaluateOptions { include_mel: false },
    )
    .await
    .expect("evaluate_graph");
    assert!(
        !evaluation.primitive_ids.is_empty(),
        "the run selected no fixtures"
    );
    assert!(evaluation.views.contains_key("view_value"));

    // Exactly what `run_graph(agentThreadId=…)` does.
    f.graph_runs.publish_for_test(&thread, Arc::new(evaluation));

    let scope = PythonScopeInput {
        graph_definition: Some(serde_json::to_value(&graph).unwrap()),
        ..f.scope()
    };
    let out = f
        .run_scoped(
            &thread,
            "import numpy as np\n\
             view = luma.graph.run.views[\"view_value\"]\n\
             vals = np.asarray(view.values)\n\
             times = np.asarray(view.times_s)\n\
             kicks = np.asarray(luma.features.drum_onsets[\"kick\"].values)\n\
             print(vals.shape, times.shape, kicks.shape)\n\
             peak_t = float(times[int(np.argmax(vals[0, :, 0]))])\n\
             float(np.min(np.abs(kicks - peak_t)))\n",
            scope,
        )
        .await;
    let distance = repr_f64(&out, "peak vs onsets");
    // The ramp peaks at the end of the span; the last kick is at 2.5s.
    assert!(
        (distance - (4.0 - KICKS[KICKS.len() - 1])).abs() < 0.05,
        "distance {distance}, stdout {}",
        out.stdout
    );
    assert!(out.stdout.contains("(3,)"), "stdout: {}", out.stdout);

    f.service.shutdown_all();
}

// ---------------------------------------------------------------------------
// §22.17 — cancellation reaches the cell
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_thread_interrupts_its_busy_cell() {
    let Some(f) = Fixture::new("cancelling_a_thread_interrupts_its_busy_cell").await else {
        return;
    };
    let thread = f.thread("track_copilot").await;
    // Warm the kernel, so the cancel below races the loop and not the startup.
    expect_ok(&f.run(&thread, "keep = 5").await, "warm up");

    let canceller = async {
        // The cell must actually be running before the cancel lands.
        for _ in 0..200 {
            if cancel_python_cell_inner(&f.service, &thread) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    };

    let (out, cancelled) = tokio::join!(f.run(&thread, "while True:\n    pass\n"), canceller);
    assert!(cancelled, "nothing was in flight to cancel");
    assert_eq!(out.status, "interrupted", "{out:?}");
    assert!(
        out.duration_ms < 30_000,
        "cancel took {}ms",
        out.duration_ms
    );

    // The namespace survived the interrupt, and the slot is free again.
    let out = f.run(&thread, "keep").await;
    expect_ok(&out, "after interrupt");
    assert_eq!(out.repr.as_deref(), Some("5"));
    assert!(!cancel_python_cell_inner(&f.service, &thread));

    f.service.shutdown_all();
}

// ---------------------------------------------------------------------------
// B6 — the venue facade builds a rig
// ---------------------------------------------------------------------------

/// A real definition, because `distribute` reads its QLC+ physical block: the
/// Rogue R2 Spot is 343 mm across, so two of them claim 0.686 m and a 3 m tower
/// face holds them with room to spare.
const MOVER: &str = "Chauvet/Chauvet-Rogue-R2-Spot.qxf";
const MOVER_MODE: &str = "18 Channel";
/// A measured GLB, because the socket supply is real geometry and a stub would
/// pin half the answer.
const DECK: &str = "stage_lab/stage_praticavel_2x1x1.glb";
/// The generated stick. Its two `TrussEnd`s are the only self-mating sockets in
/// this catalog, so they are what "open end" and "the gap" are measured on.
const TRUSS: &str = "truss/straight";

impl Fixture {
    /// An empty room, and a `venue_rig` thread pinned to it.
    ///
    /// Empty on purpose: the acceptance is that a *program* builds the rig, so
    /// anything seeded here would be a piece the program did not put there.
    async fn empty_venue(&self) -> (String, String) {
        let venue_id = format!("ven-empty-{}", uuid::Uuid::new_v4());
        sqlx::query("INSERT INTO venues (id, name) VALUES (?, 'Empty Room')")
            .bind(&venue_id)
            .execute(&self.pool)
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&self.pool, None)
            .await
            .unwrap();
        let thread = crate::database::local::agent_threads::create_thread(
            &self.pool,
            CreateAgentThreadInput {
                agent_kind: "venue_rig".to_string(),
                subject_kind: Some("venue".into()),
                subject_id: Some(venue_id.clone()),
                venue_id: Some(venue_id.clone()),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("create venue thread")
        .id;
        (venue_id, thread)
    }

    async fn run_in_venue(&self, thread_id: &str, venue_id: &str, code: &str) -> PythonCellResult {
        self.run_scoped(
            thread_id,
            code,
            PythonScopeInput {
                venue_id: Some(venue_id.to_string()),
                ..Default::default()
            },
        )
        .await
    }
}

/// One program builds a whole rig out of an empty room, and reads back what it
/// built through the same three channels the stage page draws from.
///
/// Nothing below is mocked: the manifest is assembled from this database, the
/// worker loads it, and every verb is one call into `services::stage_ops` —
/// the module `dispatch::handlers::stage` calls for the human page.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_python_program_builds_a_rig_from_an_empty_venue() {
    let Some(f) = Fixture::new("venue facade").await else {
        return;
    };
    let (venue_id, thread) = f.empty_venue().await;

    let out = f
        .run_in_venue(
            &thread,
            &venue_id,
            r#"
catalog = luma.venue.catalog()

# A portal, stated as a chain: a tower up, a corner, the beam across, a corner,
# a tower back down. Nothing here names a socket and nothing does arithmetic
# against a pose — the cursor carries the truth forward.
t = luma.venue.place("truss", at=(-5.5, 0.0), length=8, direction=(0, 0, 1),
                     label="left leg")
t = t.add("corner")
beam = t = t.add("truss", length=11, direction=(1, 0, 0), label="beam")
t = t.add("corner")
right = t.add("truss", length=8, direction=(0, 0, -1), label="right leg")

# Movers under the beam, hung by pointing at the face rather than naming it.
row = luma.venue.distribute(MOVER, 6, on=beam, face=(0, 0, -1), mode=MODE,
                            label="wash")

portal = luma.venue.extent(luma.venue.nodes())
legs = luma.venue.nodes(label="*leg")

(
    len(catalog.pieces) > 0,
    round(beam.size[0], 2),
    abs(portal.centre[0]) < 0.5,
    round(right.at[0] - beam.at[0], 2) > 0,
    row.ok and len(row.fixtures) == 6,
    len(legs),
    len(luma.venue.unplaced()),
)
"#
            .replace("MOVER", &format!("{MOVER:?}"))
            .replace("MODE", &format!("{MOVER_MODE:?}"))
            .as_str(),
        )
        .await;
    expect_ok(&out, "build the rig");
    let repr = out.repr.clone().unwrap_or_default();
    assert!(
        repr.starts_with("(True, 11.0, True, True, True, 2, 0)"),
        "the rig did not come out as asked: {repr}\n{}",
        out.stdout
    );

    // The tree names the relations, and the map draws the room. Two channels,
    // one solve apiece — and the four movers exist only because `distribute`
    // put them there.
    let out = f
        .run_in_venue(
            &thread,
            &venue_id,
            "tree = luma.venue.describe()\n\
             plan = luma.venue.tiles()\n\
             (tree.count('truss/straight'), tree.count('fixture'), \
              'unplaced: none' in tree, len(plan.splitlines()) > 3, \
              '+v toward the crowd' in tree)",
        )
        .await;
    expect_ok(&out, "describe the rig");
    assert_eq!(
        out.repr.as_deref(),
        Some("(3, 6, True, True, True)"),
        "{}",
        out.stdout
    );

    // And the sets: nobody grouped anything by hand, so every one of these is
    // derived from where the movers ended up. A venue with lights in it has
    // groups, which is the whole point of deriving them.
    let out = f
        .run_in_venue(
            &thread,
            &venue_id,
            "g = luma.venue.groups()\n(len(g) > 0, any(len(n) == 6 for n in g), \
             all(n.name for n in g), all(n.origin == 'derived' for n in g), \
             len(g[0].heads) > 0)",
        )
        .await;
    expect_ok(&out, "read the group tree");
    assert_eq!(
        out.repr.as_deref(),
        Some("(True, True, True, True, True)"),
        "{}",
        out.stdout
    );

    f.service.shutdown_all();
}

/// `describe()` says which way each light points, and the word tracks the way
/// its host is hung — which is the half of a patch a tree of relations cannot
/// say and the reason "hang it on the underside" is not advice a caller can act
/// on without checking.
///
/// The same face name on the same piece means the same side of it whether the
/// piece stands on the floor or hangs from the rig — a stick flown from the
/// grid keeps the underside it had on the ground — and an aim turns the word
/// again. Measured through the facade, not restated.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn describe_says_which_way_each_light_points() {
    let Some(f) = Fixture::new("venue beam words").await else {
        return;
    };
    let (venue_id, thread) = f.empty_venue().await;

    let out = f
        .run_in_venue(
            &thread,
            &venue_id,
            &r#"
head = luma.venue.fixtures("rogue r2 spot")[0]

def beam(node_id, tree):
    line = [l for l in tree.splitlines() if node_id in l][0]
    return line.rsplit("beam=", 1)[1].split()[0]

# The same stick, once standing on the floor and once hung from the grid — the
# grid is the room's other face and is named by pointing at it. A piece hangs
# *under* a down-facing surface rather than turning over, so its underside is
# its underside either way and both rows point at the floor.
standing = luma.venue.place("truss", at=(-4.0, 0.0), length=2.0, trim=3.0)
hanging = luma.venue.place("truss", at=(4.0, 0.0), length=2.0,
                           face=(0, 0, -1), trim=6.0)
on_floor = luma.venue.distribute(head.path, 1, on=standing, face=(0, 0, -1),
                                 mode=head.mode(18))
flown = luma.venue.distribute(head.path, 1, on=hanging, face=(0, 0, -1),
                              mode=head.mode(18))

tree = luma.venue.describe()
rest = (beam(on_floor.fixtures[0].node_id, tree),
        beam(flown.fixtures[0].node_id, tree))

# An aim is stated, not dialled: point it at the crowd and read the word back.
luma.venue.aim(flown.fixtures[0], direction=(0, 1, 0))
(rest, beam(flown.fixtures[0].node_id, luma.venue.describe()))
"#,
        )
        .await;
    expect_ok(&out, "beam words");
    let repr = out.repr.clone().unwrap_or_default();
    assert_eq!(
        repr, "(('down', 'down'), 'house')",
        "the beam word did not follow the mount or the aim: {repr}\n{}",
        out.stdout
    );

    f.service.shutdown_all();
}

/// A component is a function over the same verbs, run somewhere that is not the
/// venue yet — and stamping it is copies, not a second kind of node.
///
/// The whole draft path end to end: the scratch graph never touches the room,
/// both previews answer, and seven stamps are seven rows every other verb can
/// edit. The lights the component asked for are recorded and patched at the
/// stamp, because a draft has no patch to address them in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_draft_previews_a_component_and_stamps_copies_of_it() {
    let Some(f) = Fixture::new("venue drafts").await else {
        return;
    };
    let (venue_id, thread) = f.empty_venue().await;

    let out = f
        .run_in_venue(
            &thread,
            &venue_id,
            &r#"
def portal(s, width=11, height=8):
    t = s.place("truss", at=(-width / 2, 0), length=height, direction=(0, 0, 1))
    t = t.add("corner")
    beam = t = t.add("truss", length=width, direction=(1, 0, 0))
    t = t.add("corner")
    t.add("truss", length=height, direction=(0, 0, -1))
    s.distribute(MOVER, 4, on=beam, face=(0, 0, -1), mode=MODE)

gate = luma.venue.draft(portal, width=11)

# The venue is untouched while the draft is being looked at.
untouched = len(luma.venue.nodes()) == 0
span = gate.extent
preview = gate.render(width=320, height=180)

# Three stamps, six metres apart. Each one is ordinary rows.
for i in range(3):
    luma.venue.stamp(gate, at=(0.0, 4.0 + 6.0 * i))

rig = luma.venue.extent(luma.venue.nodes())
rows = luma.venue.nodes(kind="fixture")
(untouched, span.count, round(span.size[0], 1), round(rig.size[1], 1),
 len(rows), len(luma.venue.unplaced()))
"#
            .replace("MOVER", &format!("{MOVER:?}"))
            .replace("MODE", &format!("{MOVER_MODE:?}")),
        )
        .await;
    expect_ok(&out, "draft and stamp");
    let repr = out.repr.clone().unwrap_or_default();
    // Five pieces in the draft; three portals twelve metres apart in v; and a
    // row of four heads under each.
    assert!(
        repr.starts_with("(True, 5, 11.7, 12.3,"),
        "the draft did not preview as asked: {repr}\n{}",
        out.stdout
    );
    assert!(
        repr.ends_with(", 12, 0)"),
        "the stamps did not land: {repr}"
    );

    f.service.shutdown_all();
}

/// A piece the catalog does not have is refused at the call that named it, with
/// the near misses in the message — not placed as a node with no geometry that
/// the renderer discovers later.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_piece_the_catalog_does_not_have_is_refused() {
    let Some(f) = Fixture::new("venue unknown piece").await else {
        return;
    };
    let (venue_id, thread) = f.empty_venue().await;

    let out = f
        .run_in_venue(
            &thread,
            &venue_id,
            "try:\n\
            \x20   luma.venue.place(\"trusss\", at=(0, 0))\n\
            \x20   refusal = None\n\
             except luma.VenueRefused as error:\n\
            \x20   refusal = str(error)\n\
             (refusal, luma.venue.describe().count('\\n'))",
        )
        .await;
    expect_ok(&out, "unknown piece");
    let repr = out.repr.clone().unwrap_or_default();
    assert!(
        repr.contains("neither a catalog piece nor a fixture") && repr.contains("truss"),
        "the refusal did not name what the catalog has: {repr}"
    );
    // Nothing was written: an empty room describes as root plus its two
    // always-present sections.
    assert!(
        repr.ends_with(", 4)"),
        "a refused place left rows behind: {repr}"
    );

    f.service.shutdown_all();
}

// ---------------------------------------------------------------------------
// B6 — the usability run
// ---------------------------------------------------------------------------

/// Run an outside agent's program against an empty venue, cell by cell.
///
/// Not an assertion: the gauntlet's §8 usability run is evidence, not a
/// contract, so this drives the same facade the tests above do and writes down
/// what the room said. Inert unless `LUMA_USABILITY_IN` names a directory of
/// `cell_*.py`; results land beside them in `LUMA_USABILITY_OUT`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn venue_usability_run() {
    let Ok(input) = std::env::var("LUMA_USABILITY_IN") else {
        return;
    };
    let out_dir =
        std::path::PathBuf::from(std::env::var("LUMA_USABILITY_OUT").expect("LUMA_USABILITY_OUT"));
    std::fs::create_dir_all(&out_dir).unwrap();
    let Some(f) = Fixture::new("venue usability").await else {
        panic!("the cell service would not start");
    };
    let (venue_id, thread) = f.empty_venue().await;

    let mut cells: Vec<_> = std::fs::read_dir(&input)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "py"))
        .collect();
    cells.sort();

    let mut transcript = String::new();
    for cell in &cells {
        let code = std::fs::read_to_string(cell).unwrap();
        let name = cell.file_name().unwrap().to_string_lossy().to_string();
        let out = f.run_in_venue(&thread, &venue_id, &code).await;
        transcript.push_str(&format!(
            "===== {name} =====\n{code}\n----- status: {} -----\nstdout:\n{}\n\
             stderr:\n{}\nrepr: {:?}\ntraceback: {:?}\nnotices: {:?}\nfigures: {:?}\n\n",
            out.status,
            out.stdout,
            out.stderr,
            out.repr,
            out.traceback,
            out.notices,
            out.figures
                .iter()
                .map(|fig| fig.artifact_rel.clone())
                .collect::<Vec<_>>()
        ));
    }

    let tail = f
        .run_in_venue(
            &thread,
            &venue_id,
            "shot = luma.venue.render(view='front', width=1280, height=720)\n\
             top = luma.venue.render(view='overhead', width=1280, height=720)\n\
             print(luma.venue.describe())\n\
             print(luma.venue.tiles())\n\
             (str(shot.path), str(top.path))",
        )
        .await;
    transcript.push_str(&format!(
        "===== final =====\nstatus: {}\n{}\nrepr: {:?}\ntraceback: {:?}\n",
        tail.status, tail.stdout, tail.repr, tail.traceback
    ));
    std::fs::write(out_dir.join("transcript.txt"), &transcript).unwrap();
    for figure in &tail.figures {
        let name = std::path::Path::new(&figure.artifact_rel)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "figure.png".into());
        std::fs::write(out_dir.join(format!("{name}.b64")), &figure.base64_png).unwrap();
    }
    f.service.shutdown_all();
}

/// The library is searchable and its answer is what `distribute` is named out
/// of, and an aim written in degrees comes back as a degree.
///
/// Two verbs, one program, because they are two halves of the same sentence:
/// pick a head out of the library, hang a row of them, point one somewhere its
/// mount does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_library_names_a_head_and_an_aim_points_it() {
    let Some(f) = Fixture::new("venue library and aim").await else {
        return;
    };
    let (venue_id, thread) = f.empty_venue().await;

    let out = f
        .run_in_venue(
            &thread,
            &venue_id,
            &r#"
# The library, searched the way a person says the name.
found = luma.venue.fixtures("rogue r2 spot")
head = found[0]

# A tower, and a row of that head down its downstage face.
tower = luma.venue.place("truss", at=(-3.0, -2.0), length=3.0, direction=(0, 0, 1))
row = luma.venue.distribute(head, 4, on=tower, face=(0, 1, 0), mode=head.mode(18))
along = [f.along_m for f in row.fixtures]

# Point the whole row at one place in the room. Each head gets its own turn,
# solved from where that head actually hangs.
aimed = luma.venue.aim(row.fixtures, at=(0.0, 6.0, 0.0))
tree = luma.venue.describe()
line = [l for l in tree.splitlines() if row.fixtures[0].node_id in l][0]

# The row comes back in face order, which is what indexing it means.
(head.path, head.moves, head.mode(18), row.ok, along == sorted(along),
 len(aimed) == 4, "beam=house" in line)
"#,
        )
        .await;
    expect_ok(&out, "library and aim");
    let repr = out.repr.clone().unwrap_or_default();
    assert!(
        repr.starts_with(&format!(
            "('{MOVER}', True, '{MOVER_MODE}', True, True, True, True)"
        )),
        "the library or the aim did not answer as asked: {repr}\n{}",
        out.stdout
    );

    f.service.shutdown_all();
}

/// The second of the design's two hard errors, from Python: an extend longer
/// than the gap the ray measured is refused, changes nothing, and says by how
/// much. Asking for exactly the gap bridges it and closes both ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_extend_past_the_measured_gap_is_refused() {
    let Some(f) = Fixture::new("venue refusal").await else {
        return;
    };
    let (venue_id, thread) = f.empty_venue().await;

    let out = f
        .run_in_venue(
            &thread,
            &venue_id,
            &r#"
a = luma.venue.place("truss", at=(-3.0, 0.0), length=2.0, label="A")
b = luma.venue.place("truss", at=(3.0, 0.0), length=2.0, label="B")
gap = luma.venue.reach(a, "end_b").gap_m
before = luma.venue.describe()
try:
    luma.venue.extend(a, "end_b", gap + 1.0)
    refusal = None
except luma.VenueRefused as error:
    refusal = str(error)
(gap, refusal, luma.venue.describe() == before)
"#,
        )
        .await;
    expect_ok(&out, "refused extend");
    let repr = out.repr.clone().unwrap_or_default();
    assert!(
        repr.starts_with("(4.0, '5.00 m is longer than the 4.00 m gap"),
        "the refusal did not measure the room: {repr}"
    );
    assert!(
        repr.ends_with("True)"),
        "a refused extend changed the rig: {repr}"
    );

    // Exactly the gap bridges it: one edge, one far-end check, and neither end
    // is dangling any more.
    let out = f
        .run_in_venue(
            &thread,
            &venue_id,
            "bridge = luma.venue.extend(a, \"end_b\")\n\
             open_ends = {f'{d.node_id}.{d.socket}' for d in luma.venue.dangling()}\n\
             (bridge.placed, f'{a.node_id}.end_b' in open_ends, \
              f'{b.node_id}.end_a' in open_ends, 'satisfied' in bridge.describe())",
        )
        .await;
    expect_ok(&out, "bridging extend");
    assert_eq!(
        out.repr.as_deref(),
        Some("(True, False, False, True)"),
        "{}",
        out.stdout
    );

    f.service.shutdown_all();
}
