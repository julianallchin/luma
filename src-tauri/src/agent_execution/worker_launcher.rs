//! How a worker process gets started — the one seam the sandbox plugs into.
//!
//! Everything above this module (the protocol client, the workspace registry)
//! only ever sees a [`Child`] with three pipes. Whether that child was wrapped
//! in a macOS Seatbelt profile, a Linux Landlock launcher, or nothing at all is
//! this module's business alone (design §17.4/§17.5, decision D9).
//!
//! Two invariants every launcher must uphold, sandbox or not:
//!
//! - the child runs in its own process group, so the interruption ladder in
//!   [`super::worker_process`] can signal the whole tree at once (§16.2);
//! - the environment is an allowlist, never an inheritance (§17.2).

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::agent_execution::artifacts::SCRATCH_DIR;
use crate::cmd_util::new_process_group;

/// Environment variable that unlocks the unsandboxed launcher in a release
/// build. Debug builds don't need it; production without a real sandbox is a
/// hard stop (design §17.7).
pub const UNSANDBOXED_ENV: &str = "LUMA_UNSANDBOXED_PYTHON=1";
const UNSANDBOXED_VAR: &str = "LUMA_UNSANDBOXED_PYTHON";

/// The filesystem and environment authority one worker gets.
///
/// A launcher translates this into whatever its platform can enforce; the
/// passthrough launcher enforces only the environment half.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// The thread's workspace root. `inputs/` and `outputs/` are read-only to
    /// the worker, `scratch/` is its writable area and its cwd.
    pub workspace_dir: PathBuf,
    /// Directory holding the deployed `luma_exec` package (read + execute).
    pub python_root: PathBuf,
    /// The managed virtualenv root (read + execute).
    pub venv_dir: PathBuf,
    /// Extra read-only roots a platform launcher must grant (e.g. a bundled
    /// ffmpeg, model caches). Empty by default: everything the worker needs is
    /// supposed to arrive as an input artifact.
    pub extra_read_roots: Vec<PathBuf>,
    /// Extra allowlisted environment variables, appended after the base set.
    pub env: Vec<(String, String)>,
}

impl SandboxPolicy {
    pub fn new(workspace_dir: PathBuf, python_root: PathBuf, venv_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            python_root,
            venv_dir,
            extra_read_roots: Vec::new(),
            env: Vec::new(),
        }
    }

    /// The worker's cwd and only writable directory.
    pub fn scratch_dir(&self) -> PathBuf {
        self.workspace_dir.join(SCRATCH_DIR)
    }
}

/// Starts one worker process under some capability policy.
pub trait WorkerLauncher: Send + Sync {
    fn launch(
        &self,
        python_bin: &Path,
        worker_script: &Path,
        policy: &SandboxPolicy,
    ) -> io::Result<Child>;

    /// Human-readable name, surfaced in logs and crash diagnostics.
    fn name(&self) -> &'static str;
}

/// The base command every launcher builds on: cwd in scratch, scrubbed
/// environment, piped stdio, own process group.
///
/// A sandboxing launcher wraps the *program* (e.g. `sandbox-exec -f profile
/// <python> …`) but keeps this environment and stdio shape.
pub fn base_command(python_bin: &Path, worker_script: &Path, policy: &SandboxPolicy) -> Command {
    let scratch = policy.scratch_dir();
    let mut cmd = Command::new(python_bin);
    cmd.arg(worker_script)
        .arg("--workspace")
        .arg(&policy.workspace_dir)
        .current_dir(&scratch)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Allowlist only — no inheritance (design §17.2). HOME, TMPDIR and the
    // matplotlib config dir all point inside scratch so nothing the analysis
    // stack writes at import time escapes the workspace.
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("MPLBACKEND", "Agg")
        .env("HOME", &scratch)
        .env("TMPDIR", &scratch)
        .env("MPLCONFIGDIR", scratch.join(".matplotlib"))
        .env("LC_ALL", "en_US.UTF-8");
    for (key, value) in &policy.env {
        cmd.env(key, value);
    }

    new_process_group(&mut cmd);
    cmd
}

/// No sandbox at all: developer-only, and a hard error in release builds unless
/// the operator opts in explicitly (design §17.7, decision D9).
#[derive(Debug, Clone, Copy)]
pub struct PassthroughLauncher {
    _private: (),
}

impl PassthroughLauncher {
    pub fn new() -> Result<Self, String> {
        if cfg!(debug_assertions) || std::env::var(UNSANDBOXED_VAR).as_deref() == Ok("1") {
            return Ok(Self { _private: () });
        }
        Err(format!(
            "refusing to run agent Python without a sandbox in a release build; \
             set {UNSANDBOXED_ENV} to override for local experiments"
        ))
    }
}

impl WorkerLauncher for PassthroughLauncher {
    fn launch(
        &self,
        python_bin: &Path,
        worker_script: &Path,
        policy: &SandboxPolicy,
    ) -> io::Result<Child> {
        std::fs::create_dir_all(policy.scratch_dir())?;
        base_command(python_bin, worker_script, policy).spawn()
    }

    fn name(&self) -> &'static str {
        "passthrough"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(dir: &Path) -> SandboxPolicy {
        SandboxPolicy::new(dir.to_path_buf(), dir.join("py"), dir.join("venv"))
    }

    #[test]
    fn scratch_is_the_worker_cwd() {
        let p = policy(Path::new("/tmp/ws"));
        assert_eq!(p.scratch_dir(), Path::new("/tmp/ws/scratch"));
    }

    #[test]
    fn base_command_scrubs_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = policy(dir.path());
        p.env.push(("LUMA_TEST".into(), "1".into()));
        let cmd = base_command(Path::new("/usr/bin/python3"), Path::new("worker.py"), &p);
        let envs: Vec<_> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(envs.iter().any(|(k, _)| k == "PYTHONUNBUFFERED"));
        assert!(
            envs.iter()
                .any(|(k, v)| k == "HOME"
                    && v.as_deref() == Some(&*p.scratch_dir().to_string_lossy()))
        );
        assert!(envs
            .iter()
            .any(|(k, v)| k == "LUMA_TEST" && v.as_deref() == Some("1")));
        // env_clear() means nothing else leaks in.
        assert!(!envs.iter().any(|(k, _)| k == "PYTHONPATH"));
    }

    #[test]
    fn passthrough_is_available_in_debug_builds() {
        // The test binary is a debug build, so this is the developer path.
        assert!(PassthroughLauncher::new().is_ok());
    }
}
