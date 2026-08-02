//! Rust ↔ real-Python integration tests (design §21.5 / §21.6, host side).
//!
//! These drive an actual worker subprocess with the app venv interpreter, so
//! they also cross-validate the Rust manifest writer against the Python
//! manifest loader. Machines without the managed venv skip: the venv is created
//! by the app at runtime, not by `cargo test`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::agent_execution::artifacts::{ArtifactEncoding, ArtifactKind, ImportRequest};
use crate::agent_execution::bindings::manifest::{
    AgentKind, AnalysisScope, AnalysisWindow, AxisSpec, BindingManifest, DType, Provenance,
    TensorRef,
};
use crate::agent_execution::bindings::BindingBuilder;
use crate::agent_execution::sandbox;
use crate::agent_execution::worker_process::{CancelToken, ExecStatus, HostCallError};
use crate::agent_execution::workspace::{
    CellOutcome, PythonWorkspaceService, WorkerEnv, Workspace,
};
use crate::audio::cache::write_pcm_file;

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const FRAMES: usize = 480;
const CELL: Duration = Duration::from_secs(60);

/// The interpreter the app manages. `None` on a machine that never ran Luma.
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

struct Fixture {
    _tmp: tempfile::TempDir,
    _service: PythonWorkspaceService,
    workspace: Arc<Workspace>,
    /// Host-side source files, deliberately outside the workspace.
    sources: tempfile::TempDir,
}

/// Build a service + one workspace, or `None` when this machine has no venv.
fn fixture(name: &str) -> Option<Fixture> {
    let Some(python_bin) = venv_python() else {
        eprintln!("[skip] {name}: no managed python env at ~/Library/Caches/com.luma.luma");
        return None;
    };
    let tmp = tempfile::tempdir().unwrap();
    let env = WorkerEnv::new(
        python_bin,
        worker_script(),
        Arc::new(sandbox::default_launcher),
    );
    let service = PythonWorkspaceService::with_env(tmp.path().to_path_buf(), env);
    let workspace = service.workspace_for("thread-under-test").unwrap();
    Some(Fixture {
        _tmp: tmp,
        _service: service,
        workspace,
        sources: tempfile::tempdir().unwrap(),
    })
}

impl Fixture {
    /// One binding revision: a beats tensor (raw_le) plus a stereo mix
    /// (pcm_f32, imported from a host file) and a scalar title.
    fn manifest(&self, title: &str, beats: &[f32]) -> BindingManifest {
        let pcm_path = self.sources.path().join(format!("mix-{title}.pcm"));
        let samples: Vec<f32> = (0..FRAMES * CHANNELS as usize)
            .map(|i| (i as f32) / 1000.0)
            .collect();
        write_pcm_file(&pcm_path, &samples, SAMPLE_RATE, CHANNELS).unwrap();

        let (beats_desc, mix_desc) = self.workspace.with_store(|store| {
            let beats_desc = store.write_raw_f32(beats).unwrap();
            let mix_desc = store
                .import(ImportRequest::new(
                    &pcm_path,
                    ArtifactKind::Tensor,
                    ArtifactEncoding::PcmF32,
                ))
                .unwrap();
            (beats_desc, mix_desc)
        });

        let mut builder = BindingBuilder::new(
            AgentKind::TrackCopilot,
            AnalysisScope {
                track_id: Some("track-under-test".into()),
                window: Some(AnalysisWindow {
                    start_s: 0.0,
                    end_s: 10.0,
                }),
                ..Default::default()
            },
        );
        builder
            .artifacts([beats_desc.clone(), mix_desc.clone()])
            .unwrap();
        builder.inline("track.title", title).unwrap();
        builder
            .tensor(
                "features.beats",
                TensorRef::new(
                    beats_desc.id.clone(),
                    DType::F32,
                    vec![beats.len()],
                    vec![AxisSpec::index("event", beats.len())],
                    Provenance::new("beat_this").with_version("3"),
                )
                .with_unit("s"),
            )
            .unwrap();
        builder
            .tensor(
                "audio.mix",
                TensorRef::new(
                    mix_desc.id.clone(),
                    DType::F32,
                    vec![FRAMES, CHANNELS as usize],
                    vec![
                        AxisSpec::linear_unit(
                            "time",
                            0.0,
                            1.0 / f64::from(SAMPLE_RATE),
                            FRAMES,
                            "s",
                        ),
                        AxisSpec::labels("channel", vec!["l".into(), "r".into()]),
                    ],
                    Provenance::new("mix_pcm"),
                )
                .with_offset(crate::audio::cache::PCM_HEADER_LEN as u64),
            )
            .unwrap();
        builder.build().unwrap()
    }

    fn install(&self, title: &str, beats: &[f32]) {
        let manifest = self.manifest(title, beats);
        self.workspace.install_revision(&manifest).unwrap();
    }

    fn run(&self, code: &str) -> CellOutcome {
        self.workspace.run_cell(code, CELL, &CancelToken::new())
    }
}

fn expect_ok(outcome: &CellOutcome, what: &str) {
    assert!(
        outcome.status.is_ok(),
        "{what}: status {:?}\nstdout: {}\nstderr: {}\ntraceback: {:?}",
        outcome.status,
        outcome.stdout,
        outcome.stderr,
        outcome.traceback
    );
}

// ---------------------------------------------------------------------------
// §21.5 kernel semantics
// ---------------------------------------------------------------------------

#[test]
fn kernel_semantics_over_the_real_protocol() {
    let Some(fx) = fixture("kernel_semantics_over_the_real_protocol") else {
        return;
    };
    fx.install("Alpha", &[0.5, 1.0, 1.5, 2.0]);

    // Last-expression display.
    let out = fx.run("2+2");
    expect_ok(&out, "2+2");
    assert_eq!(out.repr.as_deref(), Some("4"));
    assert_eq!(out.kernel_generation, 1);

    // Variables persist across cells.
    expect_ok(&fx.run("x = 41"), "assign");
    let out = fx.run("x + 1");
    expect_ok(&out, "read back");
    assert_eq!(out.repr.as_deref(), Some("42"));

    // The manifest Rust wrote is the namespace Python sees.
    let out = fx.run(
        "import json; print(json.dumps({\
           'title': luma.track.title, \
           'beats': [round(float(v), 3) for v in luma.features.beats.values.tolist()], \
           'unit': luma.features.beats.unit, \
           'mix_shape': list(luma.audio.mix.values.shape), \
           'sr': luma.audio.mix.sample_rate_hz}))",
    );
    expect_ok(&out, "luma round-trip");
    let seen: serde_json::Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(seen["title"], "Alpha");
    assert_eq!(seen["beats"], serde_json::json!([0.5, 1.0, 1.5, 2.0]));
    assert_eq!(seen["unit"], "s");
    assert_eq!(seen["mix_shape"], serde_json::json!([FRAMES, CHANNELS]));
    assert_eq!(seen["sr"].as_f64(), Some(f64::from(SAMPLE_RATE)));

    // Revision refresh: new binding values, same user variables (§13.2).
    fx.install("Beta", &[9.0, 8.0]);
    let out = fx.run(
        "import json; print(json.dumps({\
           'title': luma.track.title, \
           'beats': [float(v) for v in luma.features.beats.values.tolist()], \
           'x': x}))",
    );
    expect_ok(&out, "revision refresh");
    let seen: serde_json::Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(seen["title"], "Beta");
    assert_eq!(seen["beats"], serde_json::json!([9.0, 8.0]));
    assert_eq!(seen["x"], 41);
    assert_eq!(out.kernel_generation, 1, "no restart across revisions");

    // A native fd-level write cannot corrupt the protocol (§14.5).
    let out = fx.run("import os; os.write(1, b'raw bytes not json\\n'); print('after')");
    expect_ok(&out, "fd corruption");
    assert!(
        out.stdout.contains("raw bytes not json") && out.stdout.contains("after"),
        "stdout was {:?}",
        out.stdout
    );

    // A Python-level error is an `error`, not an infra failure, and the kernel
    // survives it.
    let out = fx.run("print('before'); 1/0");
    assert_eq!(out.status, ExecStatus::Error);
    assert!(out.stdout.contains("before"));
    assert!(out
        .traceback
        .unwrap_or_default()
        .contains("ZeroDivisionError"));
    assert!(fx.workspace.is_kernel_alive());

    // Figures land in outputs/ with their real pixel dimensions (§14.7).
    let out = fx.run("plt.figure(figsize=(4, 2), dpi=100); plt.plot([1, 2, 3])");
    expect_ok(&out, "figure");
    assert_eq!(out.figures.len(), 1, "figures: {:?}", out.figures);
    let figure = &out.figures[0];
    assert_eq!((figure.width, figure.height), (400, 200));
    let png = fx.workspace.dir().join(&figure.artifact_rel);
    assert!(png.is_file(), "missing {}", png.display());
    assert!(std::fs::metadata(&png).unwrap().len() > 0);
    // The store accepts what the worker reported.
    fx.workspace
        .with_store(|store| {
            store.register_output(
                &figure.artifact_rel,
                ArtifactKind::Figure,
                ArtifactEncoding::Png,
            )
        })
        .unwrap();

    // `ping` answers with the worker's own pid (appendix A.1).
    let pong = fx.workspace.ping(Duration::from_secs(10)).unwrap().unwrap();
    assert!(pong > 0);

    // A graceful shutdown ends with `goodbye`, not a kill.
    fx.workspace.shutdown();
    assert!(!fx.workspace.is_kernel_alive());
}

#[test]
fn scoped_host_calls_round_trip_without_owning_the_kernel() {
    let Some(fx) = fixture("scoped_host_calls_round_trip_without_owning_the_kernel") else {
        return;
    };
    fx.install("Alpha", &[1.0]);

    let seen = std::sync::Mutex::new(Vec::new());
    let handler =
        |method: &str,
         payload: serde_json::Value,
         _context: &crate::agent_execution::worker_process::HostCallContext| {
            seen.lock()
                .unwrap()
                .push((method.to_string(), payload.clone()));
            Ok(serde_json::json!({
                "answer": payload["value"].as_i64().unwrap_or_default() + 1,
            }))
        };
    let out = fx.workspace.run_cell_with_host(
        "reply = _luma_host_call('test.increment', {'value': 41})\nreply['answer']",
        CELL,
        &CancelToken::new(),
        &handler,
    );
    expect_ok(&out, "host call");
    assert_eq!(out.repr.as_deref(), Some("42"));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![("test.increment".into(), serde_json::json!({"value": 41}))]
    );

    // A cell without a capability table gets a Python-level error, not a
    // protocol deadlock or kernel restart.
    let out = fx.run("_luma_host_call('test.increment', {'value': 1})");
    assert_eq!(out.status, ExecStatus::Error, "{out:?}");
    assert!(out
        .traceback
        .as_deref()
        .unwrap_or_default()
        .contains("host calls are not available"));
    expect_ok(&fx.run("6 * 7"), "kernel after unavailable host call");

    // Domain errors retain a stable code on the Python exception and likewise
    // leave the namespace intact.
    let reject =
        |_method: &str,
         _payload: serde_json::Value,
         _context: &crate::agent_execution::worker_process::HostCallContext| {
            Err(HostCallError::new("conflict", "the track changed"))
        };
    let out = fx.workspace.run_cell_with_host(
        "kept = 9\n_luma_host_call('track.apply', {})",
        CELL,
        &CancelToken::new(),
        &reject,
    );
    assert_eq!(out.status, ExecStatus::Error, "{out:?}");
    let traceback = out.traceback.unwrap_or_default();
    assert!(traceback.contains("LumaHostCallError"), "{traceback}");
    assert!(traceback.contains("the track changed"), "{traceback}");
    let out = fx.run("kept");
    expect_ok(&out, "namespace after host rejection");
    assert_eq!(out.repr.as_deref(), Some("9"));
}

#[test]
fn cancellation_reaches_a_running_host_call() {
    let Some(fx) = fixture("cancellation_reaches_a_running_host_call") else {
        return;
    };
    fx.install("Alpha", &[1.0]);
    expect_ok(&fx.run("1"), "warm host-call kernel");

    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        trigger.cancel();
    });
    let handler =
        |_method: &str,
         _payload: serde_json::Value,
         context: &crate::agent_execution::worker_process::HostCallContext| {
            while !context.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            context.check()?;
            Ok(serde_json::Value::Null)
        };
    let started = std::time::Instant::now();
    let out = fx.workspace.run_cell_with_host(
        "_luma_host_call('test.wait', {})",
        CELL,
        &cancel,
        &handler,
    );
    canceller.join().unwrap();

    assert_eq!(out.status, ExecStatus::Interrupted, "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(fx.workspace.is_kernel_alive());
}

#[test]
fn cancellation_waits_for_an_irreversible_host_response_but_not_the_rest_of_the_cell() {
    let Some(fx) = fixture(
        "cancellation_waits_for_an_irreversible_host_response_but_not_the_rest_of_the_cell",
    ) else {
        return;
    };
    fx.install("Alpha", &[1.0]);
    expect_ok(&fx.run("1"), "warm commit-barrier kernel");

    let cancel = CancelToken::new();
    let entered = Arc::new(AtomicBool::new(false));
    let cancellation_sent = Arc::new(AtomicBool::new(false));
    let committed = Arc::new(AtomicBool::new(false));
    let canceller = {
        let cancel = cancel.clone();
        let entered = Arc::clone(&entered);
        let cancellation_sent = Arc::clone(&cancellation_sent);
        std::thread::spawn(move || {
            while !entered.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            cancel.cancel();
            cancellation_sent.store(true, Ordering::SeqCst);
        })
    };
    let handler = {
        let entered = Arc::clone(&entered);
        let cancellation_sent = Arc::clone(&cancellation_sent);
        let committed = Arc::clone(&committed);
        move |_method: &str,
              _payload: serde_json::Value,
              context: &crate::agent_execution::worker_process::HostCallContext| {
            context.begin_irreversible()?;
            entered.store(true, Ordering::SeqCst);
            while !cancellation_sent.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            committed.store(true, Ordering::SeqCst);
            Ok(serde_json::json!({ "committed": true }))
        }
    };

    let started = std::time::Instant::now();
    let out = fx.workspace.run_cell_with_host(
        "_luma_host_call('track.apply', {})\nwhile True:\n    pass",
        CELL,
        &cancel,
        &handler,
    );
    canceller.join().unwrap();

    assert!(committed.load(Ordering::SeqCst));
    assert_eq!(out.status, ExecStatus::Interrupted, "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(fx.workspace.is_kernel_alive());
    expect_ok(&fx.run("40 + 2"), "kernel after commit-barrier interrupt");
}

// ---------------------------------------------------------------------------
// §21.6 interruption
// ---------------------------------------------------------------------------

#[test]
fn a_busy_loop_is_interrupted_with_the_namespace_intact() {
    let Some(fx) = fixture("a_busy_loop_is_interrupted_with_the_namespace_intact") else {
        return;
    };
    fx.install("Alpha", &[1.0]);
    expect_ok(&fx.run("y = 7"), "warm up");
    let generation = fx.workspace.kernel_generation();

    let cancel = CancelToken::new();
    {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            cancel.cancel();
        });
    }
    let out = fx
        .workspace
        .run_cell("while True:\n    pass\n", CELL, &cancel);
    assert_eq!(out.status, ExecStatus::Interrupted, "{out:?}");
    assert_eq!(
        out.kernel_generation, generation,
        "SIGINT must not cost the kernel"
    );
    assert!(out.notices.is_empty(), "notices: {:?}", out.notices);
    // The cancel must land promptly, not when the 60s ceiling expires.
    assert!(
        out.duration_ms < 10_000,
        "cancel took {}ms",
        out.duration_ms
    );

    // Namespace preserved, and the SIGINT that chased the last cell cannot land
    // on this one: the worker ignores signals between cells and the host guards
    // frames by execution id (§16.1).
    let out = fx.run("y");
    expect_ok(&out, "after interrupt");
    assert_eq!(out.repr.as_deref(), Some("7"));
    assert_eq!(out.kernel_generation, generation);
}

#[test]
fn a_cell_that_blocks_sigint_is_killed_and_reported_as_state_loss() {
    let Some(fx) = fixture("a_cell_that_blocks_sigint_is_killed_and_reported_as_state_loss") else {
        return;
    };
    fx.install("Alpha", &[1.0]);
    expect_ok(&fx.run("z = 3"), "warm up");
    let generation = fx.workspace.kernel_generation();
    assert!(fx.workspace.is_kernel_alive());

    // Blocks SIGINT outright, so only the SIGKILL rung can stop it (§16.2).
    let out = fx.workspace.run_cell(
        // `pthread_sigmask` would not be enough: CPython's C handler can run on
        // any thread and the main thread still sees the flag. SIG_IGN is a real
        // block, so only the SIGKILL rung can end this cell.
        "import signal\nsignal.signal(signal.SIGINT, signal.SIG_IGN)\nwhile True:\n    pass\n",
        Duration::from_secs(2),
        &CancelToken::new(),
    );
    match &out.status {
        ExecStatus::Failed { reason } => {
            assert!(reason.contains("ignored the interrupt"), "reason: {reason}")
        }
        other => panic!("expected an infra failure, got {other:?}"),
    }
    assert!(!fx.workspace.is_kernel_alive(), "the group must be dead");

    // The next cell respawns and carries the state-loss notice (§13.4).
    let out = fx.run("'alive'");
    expect_ok(&out, "respawn");
    assert_eq!(out.repr.as_deref(), Some("'alive'"));
    assert_eq!(out.kernel_generation, generation + 1);
    assert!(
        out.notices.iter().any(|n| n.contains("restarted")),
        "notices: {:?}",
        out.notices
    );

    // A fresh kernel re-installs the binding revision without being asked.
    let out = fx.run("luma.track.title");
    expect_ok(&out, "binding after respawn");
    assert_eq!(out.repr.as_deref(), Some("'Alpha'"));

    // …and the old namespace really is gone.
    let out = fx.run("z");
    assert_eq!(out.status, ExecStatus::Error);
    assert!(out.traceback.unwrap_or_default().contains("NameError"));
}
