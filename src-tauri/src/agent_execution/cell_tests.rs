//! Full-pipeline acceptance tests (design §22): a real database, a real
//! workspace, a real Python kernel, driven through the same
//! [`run_python_cell_inner`] the Tauri command and the harness call.
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
use crate::commands::agent_execution::{
    cancel_python_cell_inner, run_python_cell_inner, run_python_cell_inner_as,
};
use crate::eval::graph_run::{evaluate_graph, EvaluateOptions};
use crate::models::agent_execution::{PythonCellResult, PythonScopeInput};
use crate::models::agent_threads::CreateAgentThreadInput;
use crate::models::node_graph::{
    Edge, Graph, GraphContext, NodeInstance, PatternArgDef, PatternArgType,
};
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

        let f = Self {
            _dir: dir,
            pool,
            storage,
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
        let (subject_kind, subject_id, score_id) = if agent_kind == "track_copilot" {
            // A track thread's durable venue and score are one indivisible
            // scope. This score is intentionally not owned by the headless
            // caller, so these analysis tests exercise the readable,
            // non-editable track binding.
            sqlx::query(
                "INSERT OR IGNORE INTO scores (id, uid, track_id, venue_id, name)
                 VALUES (?, 'reader-fixture', ?, ?, 'Analysis')",
            )
            .bind(ANALYSIS_SCORE_ID)
            .bind(TRACK_ID)
            .bind(&self.venue_id)
            .execute(&self.pool)
            .await
            .unwrap();
            ("track", TRACK_ID, Some(ANALYSIS_SCORE_ID.to_string()))
        } else {
            ("pattern", PATTERN_ID, None)
        };
        crate::database::local::agent_threads::create_thread(
            &self.pool,
            CreateAgentThreadInput {
                agent_kind: agent_kind.to_string(),
                subject_kind: Some(subject_kind.into()),
                subject_id: Some(subject_id.into()),
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
            "INSERT INTO patterns (id, name, description, is_verified)
             VALUES (?, 'Blank canvas', 'empty test graph', 1)",
        )
        .bind(PATTERN_ID)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, graph_json)
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
            thread_id.to_string(),
            code.to_string(),
            scope,
        )
        .await
        .expect("run_python_cell")
    }

    async fn run_as_owner(&self, thread_id: &str, code: &str) -> PythonCellResult {
        let mut scope = self.scope();
        scope.score_id = Some(SCORE_ID.into());
        run_python_cell_inner_as(
            &self.pool,
            &self.storage,
            &self.resource_root,
            &self.service,
            &self.graph_runs,
            thread_id.to_string(),
            code.to_string(),
            scope,
            Some("owner".into()),
        )
        .await
        .expect("run authorized Python cell")
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
    let out = f
        .run_as_owner(
            &thread,
            "edit = luma.track.edit()\n\
             added = edit.add_clip('Blank canvas', seconds=(1.0, 2.0), z=0)\n\
             checked = edit.check()\n\
             applied = edit.apply()\n\
             (bool(checked), applied.added, len(applied.clips), added.id.startswith('new:'))",
        )
        .await;
    expect_ok(&out, "apply track candidate");
    assert_eq!(out.repr.as_deref(), Some("(True, 1, 1, True)"));

    let rows = crate::database::local::scores::list_track_scores_for_score(&f.pool, SCORE_ID)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pattern_id, PATTERN_ID);
    assert!(!rows[0].id.starts_with("new:"));

    // The next manifest reflects the commit while the ordinary Python
    // namespace survives in the same durable thread.
    let out = f
        .run_as_owner(&thread, "(len(luma.track.clips), applied.added)")
        .await;
    expect_ok(&out, "refresh committed track binding");
    assert_eq!(out.repr.as_deref(), Some("(1, 1)"));

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

    let error = run_python_cell_inner_as(
        &f.pool,
        &f.storage,
        &f.resource_root,
        &f.service,
        &f.graph_runs,
        thread.clone(),
        "1 + 1".into(),
        scope,
        Some("another-user".into()),
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
    f.graph_runs.publish(&thread, Arc::new(evaluation));

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

    let cancel_target = thread.clone();
    let canceller = tokio::spawn(async move {
        // The cell must actually be running before the cancel lands.
        for _ in 0..200 {
            if cancel_python_cell_inner(&cancel_target) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    });

    let out = f.run(&thread, "while True:\n    pass\n").await;
    assert!(canceller.await.unwrap(), "nothing was in flight to cancel");
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
    assert!(!cancel_python_cell_inner(&thread));

    f.service.shutdown_all();
}
