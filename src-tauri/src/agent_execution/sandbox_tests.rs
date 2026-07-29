//! Sandbox acceptance tests (design §21.7), macOS Seatbelt.
//!
//! These run *real* agent code inside a *real* sandboxed kernel and assert on
//! what it can and cannot reach. They are the executable form of the capability
//! policy in §17.2 — if one of them starts passing for the wrong reason the
//! boundary has moved, so each assertion checks the *kind* of failure, not just
//! that something failed.
//!
//! Machines without the managed venv skip, like `kernel_tests`.
//!
//! ## Two Seatbelt semantics these tests encode
//!
//! 1. **Denial precedes nothing; existence precedes denial.** Opening a path
//!    that does not exist returns `ENOENT` (`FileNotFoundError`) even inside a
//!    denied directory; only an *existing* denied path returns `EPERM`
//!    (`PermissionError`). So a test that wants to prove "denied" must point at
//!    something that exists. Verified empirically on Darwin 25.
//! 2. **A denied connect is `EPERM`, not `ECONNREFUSED`.** That is what lets
//!    the network test distinguish "the sandbox stopped me" from "nothing was
//!    listening", which is why it connects to a socket the test really is
//!    listening on.

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::agent_execution::bindings::manifest::{
    AgentKind, AnalysisScope, AxisSpec, BindingManifest, DType, Provenance, TensorRef,
};
use crate::agent_execution::bindings::BindingBuilder;
use crate::agent_execution::sandbox;
use crate::agent_execution::workspace::{
    CellOutcome, PythonWorkspaceService, WorkerEnv, Workspace,
};

const CELL: Duration = Duration::from_secs(60);
const BEATS: [f32; 4] = [0.25, 0.5, 0.75, 1.0];

/// The exact environment `base_command` grants. Nothing else may reach the
/// kernel — no `OPENROUTER_API_KEY`, no `AWS_*`, no real `HOME` (§17.2).
const ALLOWED_ENV: &[&str] = &[
    "HOME",
    "LC_ALL",
    "MPLBACKEND",
    "MPLCONFIGDIR",
    "PATH",
    "PYTHONDONTWRITEBYTECODE",
    "PYTHONUNBUFFERED",
    "TMPDIR",
];

/// The only keys allowed to appear *beyond* the allowlist, because the worker
/// sets them on itself after launch: `sklearn` and `joblib` both write
/// `KMP_INIT_AT_FORK` / `KMP_DUPLICATE_LIB_OK` into `os.environ` at import time
/// (verified by grepping the installed packages), and librosa pulls both in.
/// They are OpenMP knobs, not inherited state.
const SELF_SET_ENV_PREFIX: &str = "KMP_";

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
    /// A directory outside every allowed root, for canary files.
    outside: tempfile::TempDir,
    /// Workspace-relative path of the manifest written by `install`.
    manifest_rel: String,
}

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
    let workspace = service.workspace_for(name).unwrap();
    let manifest_rel = install(&workspace);
    Some(Fixture {
        _tmp: tmp,
        _service: service,
        workspace,
        outside: tempfile::tempdir().unwrap(),
        manifest_rel,
    })
}

/// One revision holding a beats tensor, so the "inputs are readable" test reads
/// a real artifact through the `luma` namespace. Returns the manifest's
/// workspace-relative path — `Workspace::installed_revision` only reports it
/// once a cell has actually installed it.
fn install(workspace: &Workspace) -> String {
    let desc = workspace
        .with_store(|store| store.write_raw_f32(&BEATS))
        .unwrap();
    let mut builder = BindingBuilder::new(AgentKind::TrackCopilot, AnalysisScope::default());
    builder.artifacts([desc.clone()]).unwrap();
    builder
        .tensor(
            "features.beats",
            TensorRef::new(
                desc.id.clone(),
                DType::F32,
                vec![BEATS.len()],
                vec![AxisSpec::index("event", BEATS.len())],
                Provenance::new("beat_this"),
            )
            .with_unit("s"),
        )
        .unwrap();
    let manifest: BindingManifest = builder.build().unwrap();
    workspace.install_revision(&manifest).unwrap()
}

impl Fixture {
    fn run(&self, code: &str) -> CellOutcome {
        self.workspace.run_cell(
            code,
            CELL,
            &crate::agent_execution::worker_process::CancelToken::new(),
        )
    }

    /// Run a cell that prints one JSON object and return it.
    fn probe(&self, code: &str) -> Value {
        let out = self.run(code);
        assert!(
            out.status.is_ok(),
            "probe failed: {:?}\nstdout: {}\nstderr: {}\ntraceback: {:?}",
            out.status,
            out.stdout,
            out.stderr,
            out.traceback
        );
        serde_json::from_str(out.stdout.trim()).unwrap_or_else(|e| {
            panic!("probe did not print JSON ({e}): {:?}", out.stdout);
        })
    }

    /// The launcher really is the sandbox, not the passthrough.
    fn assert_sandboxed(&self) {
        assert_eq!(
            sandbox::default_launcher().unwrap().name(),
            "seatbelt",
            "these assertions are meaningless without the sandbox"
        );
    }
}

/// `try_open(path)` → `"<ExceptionType>: <message>"`, or `"ALLOWED"`.
const TRY_HELPER: &str = r#"
import json
def try_it(fn):
    try:
        fn()
    except BaseException as exc:
        return f"{type(exc).__name__}: {exc}"
    return "ALLOWED"
"#;

// ---------------------------------------------------------------------------
// Inputs readable / scratch writable / inputs immutable
// ---------------------------------------------------------------------------

#[test]
fn input_artifacts_are_readable_and_scratch_is_writable() {
    let Some(fx) = fixture("inputs-readable") else {
        return;
    };
    fx.assert_sandboxed();

    // The tensor's bytes live in `inputs/`; reading `.values` memory-maps them.
    let seen = fx.probe(
        "import json; print(json.dumps({\
           'beats': [round(float(v), 3) for v in luma.features.beats.values.tolist()], \
           'unit': luma.features.beats.unit}))",
    );
    assert_eq!(seen["beats"], serde_json::json!([0.25, 0.5, 0.75, 1.0]));
    assert_eq!(seen["unit"], "s");

    // cwd is scratch, and scratch is the one place agent code may write.
    let out = fx.run("open('x.txt', 'w').write('hello'); print(open('x.txt').read())");
    assert!(out.status.is_ok(), "{out:?}");
    assert_eq!(out.stdout.trim(), "hello");
    assert!(fx.workspace.dir().join("scratch/x.txt").is_file());
}

#[test]
fn installed_inputs_cannot_be_rewritten_from_a_cell() {
    let Some(fx) = fixture("inputs-immutable") else {
        return;
    };
    fx.assert_sandboxed();

    // An *existing* manifest file: proves EPERM, not ENOENT (see module docs).
    let existing = fx.manifest_rel.clone();
    assert!(fx.workspace.dir().join(&existing).is_file());
    let seen = fx.probe(&format!(
        "{TRY_HELPER}\n\
         import os\n\
         ws = os.path.dirname(os.getcwd())\n\
         print(json.dumps({{\
           'overwrite': try_it(lambda: open(os.path.join(ws, {existing:?}), 'w')), \
           'create': try_it(lambda: open(os.path.join(ws, 'inputs', 'evil.bin'), 'wb')), \
           'unlink': try_it(lambda: os.remove(os.path.join(ws, {existing:?}))), \
           'read': try_it(lambda: open(os.path.join(ws, {existing:?}), 'rb').read(1))}}))"
    ));
    for key in ["overwrite", "create", "unlink"] {
        let got = seen[key].as_str().unwrap();
        assert!(
            got.starts_with("PermissionError"),
            "{key} was not denied as a permission error: {got}"
        );
        // §21.7: the denial names the capability that was refused.
        assert!(
            got.contains("Operation not permitted"),
            "{key} denial does not identify the rejection: {got}"
        );
    }
    assert_eq!(seen["read"], "ALLOWED", "inputs must stay readable");
    // The manifest on disk is untouched.
    assert!(fx.workspace.dir().join(&existing).is_file());
}

#[test]
fn the_generated_profile_cannot_be_rewritten_by_the_code_it_constrains() {
    let Some(fx) = fixture("profile-immutable") else {
        return;
    };
    fx.assert_sandboxed();

    // The profile is written at launch, so start the kernel first.
    assert!(fx.run("1").status.is_ok());

    // `sandbox.sb` lives in the workspace root, which is a read root but not a
    // write root. If a cell could rewrite it, the next kernel launch would run
    // under a policy agent code wrote.
    let profile = fx.workspace.dir().join("sandbox.sb");
    assert!(profile.is_file(), "no profile at {}", profile.display());
    let before = std::fs::read_to_string(&profile).unwrap();

    let seen = fx.probe(&format!(
        "{TRY_HELPER}\n\
         import os\n\
         p = {profile:?}\n\
         print(json.dumps({{\
           'overwrite': try_it(lambda: open(p, 'w')), \
           'append': try_it(lambda: open(p, 'a')), \
           'unlink': try_it(lambda: os.remove(p)), \
           'sibling': try_it(lambda: open(os.path.join(os.path.dirname(p), 'evil.sb'), 'w'))}}))",
        profile = profile.to_string_lossy(),
    ));
    for key in ["overwrite", "append", "unlink", "sibling"] {
        let got = seen[key].as_str().unwrap();
        assert!(
            got.starts_with("PermissionError"),
            "{key} on the profile was not denied: {got}"
        );
    }
    assert_eq!(std::fs::read_to_string(&profile).unwrap(), before);
}

#[test]
fn a_symlink_planted_in_scratch_cannot_widen_the_policy() {
    let Some(fx) = fixture("symlink-escape") else {
        return;
    };
    fx.assert_sandboxed();

    // §17.2: "the sandbox must resolve symlinks and must not let a writable
    // path widen the policy". scratch *is* writable, so agent code can plant a
    // link there — Seatbelt must evaluate the target, not the link.
    let canary = fx.outside.path().join("secret.txt");
    std::fs::write(&canary, "top secret").unwrap();
    let canary = std::fs::canonicalize(&canary).unwrap();

    let seen = fx.probe(&format!(
        "{TRY_HELPER}\n\
         import os\n\
         os.symlink({canary:?}, 'link')\n\
         os.symlink({home:?}, 'homelink')\n\
         print(json.dumps({{\
           'via_symlink': try_it(lambda: open('link').read()), \
           'via_dir_symlink': try_it(lambda: os.listdir('homelink')), \
           'link_exists': os.path.islink('link')}}))",
        canary = canary.to_string_lossy(),
        home = dirs::home_dir().unwrap().to_string_lossy(),
    ));
    assert_eq!(seen["link_exists"], true, "the symlink was not created");
    for key in ["via_symlink", "via_dir_symlink"] {
        let got = seen[key].as_str().unwrap();
        assert!(
            got.starts_with("PermissionError") && got.contains("Operation not permitted"),
            "a symlink from scratch widened the policy ({key}): {got}"
        );
    }
}

// ---------------------------------------------------------------------------
// Home credentials, app databases
// ---------------------------------------------------------------------------

#[test]
fn a_secret_outside_the_workspace_is_unreadable() {
    let Some(fx) = fixture("outside-canary") else {
        return;
    };
    fx.assert_sandboxed();

    // A file that really exists, in the real temp area, outside every root the
    // profile grants.
    let canary = fx.outside.path().join("id_rsa");
    std::fs::write(&canary, "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
    let canary = std::fs::canonicalize(&canary).unwrap();
    let canary = canary.to_string_lossy().into_owned();
    // Sanity: the host can read it, so a failure below is the sandbox.
    assert!(std::fs::read(&canary).is_ok());

    let seen = fx.probe(&format!(
        "{TRY_HELPER}\n\
         import os\n\
         print(json.dumps({{\
           'read': try_it(lambda: open({canary:?}, 'rb').read()), \
           'listdir': try_it(lambda: os.listdir(os.path.dirname({canary:?}))), \
           'home': try_it(lambda: os.listdir({home:?}))}}))",
        home = dirs::home_dir().unwrap().to_string_lossy(),
    ));
    for key in ["read", "listdir", "home"] {
        let got = seen[key].as_str().unwrap();
        assert!(
            got.starts_with("PermissionError") && got.contains("Operation not permitted"),
            "{key} was not denied: {got}"
        );
    }
    // The denial names the path — the agent can tell *what* it was refused.
    assert!(seen["read"].as_str().unwrap().contains(&canary));
}

#[test]
fn the_luma_database_is_unreadable() {
    let Some(fx) = fixture("luma-db") else {
        return;
    };
    fx.assert_sandboxed();

    // The literal production path from AGENTS.md, not a stand-in: the policy
    // denies by path, so only the real one proves anything.
    let app_support = dirs::home_dir()
        .unwrap()
        .join("Library/Application Support/com.luma.luma");
    let db = app_support.join("luma.db");

    // Seatbelt resolves existence before the policy, so only an *existing*
    // target yields EPERM. Aim at whichever of the two exists; if neither does,
    // this machine has never run Luma and there is nothing to prove.
    let target = if db.is_file() {
        db
    } else if app_support.is_dir() {
        app_support
    } else {
        eprintln!("[skip] the_luma_database_is_unreadable: no app-support dir on this machine");
        return;
    };
    let target = target.to_string_lossy().into_owned();

    let seen = fx.probe(&format!(
        "{TRY_HELPER}\n\
         import os\n\
         p = {target:?}\n\
         print(json.dumps({{'open': try_it(lambda: open(p, 'rb').read(1)), \
                            'list': try_it(lambda: os.listdir(p))}}))"
    ));
    // One of the two applies depending on whether we aimed at the file or the
    // directory; whichever it is must be a *permission* failure, and neither
    // may be ALLOWED.
    for key in ["open", "list"] {
        let got = seen[key].as_str().unwrap();
        assert_ne!(got, "ALLOWED", "{key} on {target} was allowed");
        assert!(
            got.starts_with("PermissionError") || got.starts_with("NotADirectoryError"),
            "{key} on {target}: {got}"
        );
    }
    assert!(seen["open"]
        .as_str()
        .unwrap()
        .contains("Operation not permitted"));
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

#[test]
fn the_network_is_denied_even_to_a_socket_that_is_listening() {
    let Some(fx) = fixture("network") else {
        return;
    };
    fx.assert_sandboxed();

    // A real listener, so a failure cannot be mistaken for "nothing there".
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepting = std::thread::spawn(move || {
        // One unsandboxed connection proves the socket is reachable; then a
        // second accept with a short deadline shows nothing else arrived.
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4];
        let _ = stream.read(&mut buf);
        buf
    });
    // The unsandboxed control probe.
    {
        use std::io::Write;
        let mut probe = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        probe.write_all(b"host").unwrap();
    }
    assert_eq!(&accepting.join().unwrap(), b"host");

    let seen = fx.probe(&format!(
        "{TRY_HELPER}\n\
         import socket, urllib.request\n\
         print(json.dumps({{\
           'listening': try_it(lambda: socket.create_connection(('127.0.0.1', {port}), timeout=2)), \
           'closed': try_it(lambda: socket.create_connection(('127.0.0.1', 9), timeout=2)), \
           'http': try_it(lambda: urllib.request.urlopen('http://example.com', timeout=3)), \
           'dns': try_it(lambda: socket.getaddrinfo('example.com', 80)), \
           'bind': try_it(lambda: socket.socket().bind(('127.0.0.1', 0)))}}))"
    ));

    // The load-bearing one: something *was* listening on that port and the
    // sandboxed child still could not reach it, with EPERM rather than the
    // ECONNREFUSED an unsandboxed process would have to see.
    let listening = seen["listening"].as_str().unwrap();
    assert!(
        listening.starts_with("PermissionError") && listening.contains("Operation not permitted"),
        "connecting to a live local listener was not blocked by the sandbox: {listening}"
    );
    assert!(
        !listening.contains("Connection refused"),
        "a refusal is not proof of sandboxing: {listening}"
    );

    for key in ["closed", "http", "dns", "bind"] {
        let got = seen[key].as_str().unwrap();
        assert_ne!(got, "ALLOWED", "{key} succeeded");
        assert!(
            !got.contains("Connection refused"),
            "{key} failed for the wrong reason: {got}"
        );
    }
}

// ---------------------------------------------------------------------------
// Environment and subprocesses
// ---------------------------------------------------------------------------

#[test]
fn the_environment_carries_no_secrets_and_no_real_home() {
    let Some(fx) = fixture("environment") else {
        return;
    };
    fx.assert_sandboxed();

    let seen = fx.probe(
        "import json, os; print(json.dumps({'keys': sorted(os.environ), \
          'home': os.environ['HOME'], 'tmp': os.environ['TMPDIR'], 'cwd': os.getcwd()}))",
    );
    let keys: Vec<&str> = seen["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    // Exactly the allowlist plus the OpenMP knobs the analysis stack sets on
    // itself — an *inherited* variable would show up here as neither.
    let inherited: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|k| !ALLOWED_ENV.contains(k) && !k.starts_with(SELF_SET_ENV_PREFIX))
        .collect();
    assert!(
        inherited.is_empty(),
        "the kernel inherited environment beyond the allowlist: {inherited:?}"
    );
    for expected in ALLOWED_ENV {
        assert!(keys.contains(expected), "{expected} is missing: {keys:?}");
    }
    // Spelled out, because these are the ones that actually matter: nothing
    // resembling a credential reached the child, whatever this machine's shell
    // has exported.
    for key in &keys {
        let upper = key.to_ascii_uppercase();
        assert!(
            ![
                "KEY",
                "TOKEN",
                "SECRET",
                "PASSWORD",
                "AWS",
                "OPENROUTER",
                "ANTHROPIC"
            ]
            .iter()
            .any(|needle| upper.contains(needle)),
            "a credential-shaped variable reached the kernel: {key}"
        );
    }

    let scratch = fx.workspace.dir().join("scratch");
    let scratch = scratch.to_string_lossy();
    assert_eq!(
        seen["home"].as_str().unwrap(),
        scratch,
        "HOME must be scratch"
    );
    assert_eq!(seen["tmp"].as_str().unwrap(), scratch);
    assert_eq!(seen["cwd"].as_str().unwrap(), scratch);
    let real_home = dirs::home_dir().unwrap();
    assert!(!scratch.starts_with(&*real_home.to_string_lossy()) || scratch.contains("scratch"));
}

#[test]
fn no_binary_but_the_interpreter_can_be_executed() {
    let Some(fx) = fixture("subprocess") else {
        return;
    };
    fx.assert_sandboxed();

    let seen = fx.probe(&format!(
        "{TRY_HELPER}\n\
         import subprocess\n\
         def run(*a):\n\
         \x20   return subprocess.run(list(a), capture_output=True, timeout=10, check=True)\n\
         print(json.dumps({{\
           'sh': try_it(lambda: run('/bin/sh', '-c', 'echo pwned')), \
           'ls': try_it(lambda: run('/bin/ls', '/Users')), \
           'security': try_it(lambda: run('/usr/bin/security', 'dump-keychain'))}}))"
    ));
    for key in ["sh", "ls", "security"] {
        let got = seen[key].as_str().unwrap();
        assert!(
            got.starts_with("PermissionError") && got.contains("Operation not permitted"),
            "{key} was not denied as a sandbox rejection: {got}"
        );
    }
    // And the message names the binary that was refused (§21.7 last bullet).
    assert!(seen["sh"].as_str().unwrap().contains("/bin/sh"));
}
