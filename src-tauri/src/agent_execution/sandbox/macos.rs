//! macOS Seatbelt sandbox for the agent Python worker (design §17.2/§17.4).
//!
//! One SBPL profile is generated per launch into `<workspace>/sandbox.sb` and
//! handed to `/usr/bin/sandbox-exec`, which compiles it, applies it to itself
//! and then `execve`s the interpreter **in place**. Two consequences worth
//! knowing:
//!
//! - the pid the host holds is the Python process (there is no intermediate
//!   process to lose signals in), and the process group `base_command` creates
//!   survives the `execve`, so the `killpg` interrupt ladder in
//!   [`super::super::worker_process`] is unchanged;
//! - a profile that fails to compile is a *launch* failure, not a silent
//!   downgrade — `sandbox-exec` exits before Python ever starts.
//!
//! ## Policy shape
//!
//! Deny by default, then grant:
//!
//! - read + exec on the interpreter (its real path, symlinks resolved) and read
//!   on the runtime root, the managed venv and the deployed `luma_exec` package;
//! - read on the thread's own workspace (its `inputs/`, manifests and outputs);
//! - write on `scratch/` and `outputs/` only — `inputs/` stays read-only, which
//!   is what makes an installed binding revision immutable from Python (§9.6);
//! - the handful of system read roots CPython's dynamic loader needs.
//!
//! Everything else — the home directory, `luma.db`, other threads' workspaces,
//! the network, executing any binary that is not the interpreter — is denied.
//!
//! ## Why each non-obvious allowance is here
//!
//! Every rule below was arrived at empirically: start from `(deny default)`,
//! run the real preload (numpy + scipy + librosa + matplotlib/Agg + a figure
//! save), read the denial, add the narrowest rule that fixes it, then subtract
//! rules one at a time to prove each is still load-bearing. The comments record
//! what breaks without each one.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Child;

use crate::agent_execution::artifacts::{OUTPUTS_DIR, SCRATCH_DIR};
use crate::agent_execution::worker_launcher::{base_command, SandboxPolicy, WorkerLauncher};

/// The system tool that compiles and applies an SBPL profile.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Where the generated profile is written, relative to the workspace root.
/// Inside the workspace but outside every writable root, so the sandboxed
/// process cannot rewrite the policy that constrains it.
const PROFILE_NAME: &str = "sandbox.sb";

/// System paths every CPython process needs to read to get off the ground.
///
/// - `/usr/lib` — `libSystem`, `libc++`, and the loader's fallback search path;
///   without it `dyld` aborts before `main`.
/// - `/System` — the dyld shared cache (`/System/Volumes/Preboot/Cryptexes/OS`
///   on current macOS), the system frameworks numpy's Accelerate BLAS links
///   against, and the system fonts matplotlib falls back to.
/// - `/usr/share` — `zoneinfo` (stdlib `zoneinfo`, pandas) and `terminfo`.
/// - `/private/var/db/dyld` — the pre-Ventura location of the shared cache.
///   Unused on this machine (Darwin 25); kept because its absence on an older
///   host would mean the interpreter never starts at all, and the directory
///   contains nothing user-specific.
const SYSTEM_READ_ROOTS: &[&str] = &["/usr/lib", "/usr/share", "/System", "/private/var/db/dyld"];

/// Character devices the stdlib and the analysis stack may open. Benign: none
/// of them can carry data out of the sandbox or reveal anything about the user.
const DEVICE_READS: &[&str] = &["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom"];

/// Launches the worker under a per-workspace Seatbelt profile.
#[derive(Debug, Clone)]
pub struct SeatbeltLauncher {
    sandbox_exec: PathBuf,
}

impl SeatbeltLauncher {
    /// Fails when `sandbox-exec` is missing. Callers must treat that as a hard
    /// stop for the Python tool (design §17.7) — never a fallback.
    pub fn new() -> Result<Self, String> {
        let sandbox_exec = PathBuf::from(SANDBOX_EXEC);
        if !sandbox_exec.is_file() {
            return Err(format!(
                "{SANDBOX_EXEC} is missing; agent Python cannot be sandboxed on this system"
            ));
        }
        Ok(Self { sandbox_exec })
    }

    /// Render the profile for one launch. Public for the acceptance tests, which
    /// assert on the generated policy directly.
    pub fn profile(&self, python_bin: &Path, policy: &SandboxPolicy) -> Result<String, String> {
        let interpreter = canonical(python_bin, "python interpreter")?;
        // `<runtime>/bin/python3.12` → `<runtime>`. The venv's `bin/python3` is
        // a symlink into the standalone runtime, and the runtime root holds the
        // stdlib and the extension modules the interpreter dlopens.
        let runtime_root = interpreter
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| format!("interpreter has no runtime root: {}", interpreter.display()))?
            .to_path_buf();

        let workspace = canonical(&policy.workspace_dir, "workspace")?;
        let package_dir = canonical(&policy.python_root, "luma_exec package")?;
        let mut read_roots = vec![
            runtime_root,
            canonical(&policy.venv_dir, "venv")?,
            package_dir.clone(),
            workspace.clone(),
        ];
        // `worker.py` puts the package's *parent* on `sys.path`, and CPython's
        // path finder lists that directory to resolve `import luma_exec`. A
        // `literal` grants the listing without granting the directory's other
        // children — in the dev tree that parent is `src-tauri/python/`, which
        // holds every other Python worker in the app.
        let mut literal_reads = vec![PathBuf::from("/")];
        if let Some(parent) = package_dir.parent() {
            literal_reads.push(parent.to_path_buf());
        }
        for extra in &policy.extra_read_roots {
            read_roots.push(canonical(extra, "extra read root")?);
        }
        for root in SYSTEM_READ_ROOTS {
            // Not canonicalized: these are stable system paths, and one that is
            // absent on this macOS version must not fail the launch.
            read_roots.push(PathBuf::from(root));
        }

        let write_roots = vec![workspace.join(SCRATCH_DIR), workspace.join(OUTPUTS_DIR)];

        // The interpreter is reachable under two names — the venv symlink the
        // host launches and the real binary `sys.executable` re-execs. Seatbelt
        // matches on the resolved path, but listing both keeps the policy honest
        // if that ever stops being true.
        let mut exec_paths = vec![interpreter];
        let as_given = python_bin.to_path_buf();
        if !exec_paths.contains(&as_given) {
            exec_paths.push(as_given);
        }

        let mut sbpl = String::new();
        let w = &mut sbpl;
        // `unwrap` on a String write cannot fail.
        writeln!(w, "(version 1)").unwrap();
        writeln!(w, "(deny default)").unwrap();
        writeln!(w, "(deny network*)").unwrap();
        writeln!(w).unwrap();
        writeln!(
            w,
            "; Single directories, listable but not recursively readable:\n\
             ;   \"/\"        CPython opens the root directory during startup;\n\
             ;              without it the process aborts before it can report why.\n\
             ;   <sys.path> the parent of the luma_exec package, which the import\n\
             ;              machinery lists to find it."
        )
        .unwrap();
        writeln!(w, "(allow file-read*").unwrap();
        for dir in &literal_reads {
            writeln!(w, "  (literal {})", quote(dir)?).unwrap();
        }
        writeln!(w, ")").unwrap();
        writeln!(w).unwrap();
        writeln!(
            w,
            "; hw.* / kern.* queries from CPython, numpy and OpenBLAS at import\n\
             ; time (page size, core count). Read-only, no user data."
        )
        .unwrap();
        writeln!(w, "(allow sysctl-read)").unwrap();
        writeln!(w).unwrap();
        writeln!(
            w,
            "; POSIX shared memory + semaphores: multiprocessing primitives that\n\
             ; joblib (via librosa) probes on import. Denying them only produces a\n\
             ; \"joblib will operate in serial mode\" warning on every kernel start;\n\
             ; neither escapes the process."
        )
        .unwrap();
        writeln!(w, "(allow ipc-posix-shm)").unwrap();
        writeln!(w, "(allow ipc-posix-sem)").unwrap();
        writeln!(w).unwrap();
        writeln!(
            w,
            "; Fork is allowed (multiprocessing), but the only thing this process\n\
             ; may ever exec is the interpreter itself — not /bin/sh, not any\n\
             ; bundled tool (design §17.2)."
        )
        .unwrap();
        writeln!(w, "(allow process-fork)").unwrap();
        writeln!(w, "(allow process-exec").unwrap();
        for path in &exec_paths {
            writeln!(w, "  (literal {})", quote(path)?).unwrap();
        }
        writeln!(w, ")").unwrap();
        writeln!(w, "(allow signal (target self))").unwrap();
        writeln!(w).unwrap();
        writeln!(w, "(allow file-read*").unwrap();
        for root in &read_roots {
            writeln!(w, "  (subpath {})", quote(root)?).unwrap();
        }
        for device in DEVICE_READS {
            writeln!(w, "  (literal \"{device}\")").unwrap();
        }
        writeln!(w, ")").unwrap();
        writeln!(w).unwrap();
        writeln!(
            w,
            "; Writable: this thread's scratch (also its cwd, HOME, TMPDIR and\n\
             ; MPLCONFIGDIR) and its outputs. `inputs/` is deliberately absent —\n\
             ; an installed binding revision is immutable from Python (§9.6)."
        )
        .unwrap();
        writeln!(w, "(allow file-write*").unwrap();
        for root in &write_roots {
            writeln!(w, "  (subpath {})", quote(root)?).unwrap();
        }
        writeln!(w, "  (literal \"/dev/null\")").unwrap();
        writeln!(w, ")").unwrap();

        Ok(sbpl)
    }
}

impl WorkerLauncher for SeatbeltLauncher {
    fn launch(
        &self,
        python_bin: &Path,
        worker_script: &Path,
        policy: &SandboxPolicy,
    ) -> io::Result<Child> {
        // The write roots must exist before the profile names them: Seatbelt
        // resolves `subpath` against the filesystem, and a missing directory
        // silently narrows the policy to nothing.
        std::fs::create_dir_all(policy.scratch_dir())?;
        std::fs::create_dir_all(policy.workspace_dir.join(OUTPUTS_DIR))?;

        let profile = self
            .profile(python_bin, policy)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let profile_path = policy.workspace_dir.join(PROFILE_NAME);
        std::fs::write(&profile_path, profile)?;

        let prefix = [
            self.sandbox_exec.as_os_str(),
            OsStr::new("-f"),
            profile_path.as_os_str(),
        ];
        base_command(&prefix, python_bin, worker_script, policy).spawn()
    }

    fn name(&self) -> &'static str {
        "seatbelt"
    }
}

/// Resolve symlinks before the path reaches the profile. A policy written
/// against an unresolved path is a policy an attacker can widen by pointing a
/// symlink somewhere else (design §17.2).
fn canonical(path: &Path, what: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(path)
        .map_err(|e| format!("cannot resolve the {what} path {}: {e}", path.display()))
}

/// SBPL string literal. Paths are attacker-influenced only through where the
/// user put their app data, but a quote or a newline in one would end the rule
/// early and could widen the policy — so escape what is escapable and refuse
/// what is not.
fn quote(path: &Path) -> Result<String, String> {
    let raw = path
        .to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))?;
    if raw.chars().any(|c| c.is_control()) {
        return Err(format!(
            "refusing to build a sandbox profile for a path containing control characters: {raw}"
        ));
    }
    let escaped = raw.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("\"{escaped}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A policy over a real temp directory, since the profile canonicalizes.
    fn fixture() -> (tempfile::TempDir, PathBuf, SandboxPolicy) {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let venv = root.join("venv");
        let bin = venv.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let python = bin.join("python3");
        std::fs::write(&python, "#!/bin/sh\n").unwrap();
        let py_pkg = root.join("luma_exec");
        std::fs::create_dir_all(&py_pkg).unwrap();
        let workspace = root.join("ws");
        for sub in ["inputs", SCRATCH_DIR, OUTPUTS_DIR] {
            std::fs::create_dir_all(workspace.join(sub)).unwrap();
        }
        let policy = SandboxPolicy::new(workspace, py_pkg, venv);
        (tmp, python, policy)
    }

    #[test]
    fn the_profile_denies_by_default_and_denies_the_network() {
        let (_tmp, python, policy) = fixture();
        let sbpl = SeatbeltLauncher::new()
            .unwrap()
            .profile(&python, &policy)
            .unwrap();
        assert!(sbpl.starts_with("(version 1)\n(deny default)\n(deny network*)\n"));
    }

    #[test]
    fn inputs_are_readable_but_never_writable() {
        let (_tmp, python, policy) = fixture();
        let sbpl = SeatbeltLauncher::new()
            .unwrap()
            .profile(&python, &policy)
            .unwrap();
        let (_, writes) = sbpl.split_once("(allow file-write*").unwrap();
        assert!(!writes.contains("inputs"), "write roots: {writes}");
        assert!(writes.contains(SCRATCH_DIR) && writes.contains(OUTPUTS_DIR));
        // The workspace read root covers inputs/ and the manifests in it.
        let reads = sbpl.split_once("(allow file-read*\n").unwrap().1;
        assert!(reads.contains(&*policy.workspace_dir.to_string_lossy()));
    }

    #[test]
    fn only_the_interpreter_may_be_executed() {
        let (_tmp, python, policy) = fixture();
        let sbpl = SeatbeltLauncher::new()
            .unwrap()
            .profile(&python, &policy)
            .unwrap();
        let execs = sbpl
            .split_once("(allow process-exec")
            .unwrap()
            .1
            .split_once(")\n(allow signal")
            .unwrap()
            .0;
        assert!(execs.contains(&*python.to_string_lossy()));
        assert!(!execs.contains("/bin/sh"));
        assert!(
            !execs.contains("subpath"),
            "exec must be per-binary: {execs}"
        );
    }

    #[test]
    fn roots_are_canonicalized_so_a_symlink_cannot_widen_the_policy() {
        let (tmp, python, mut policy) = fixture();
        let real = std::fs::canonicalize(tmp.path()).unwrap().join("elsewhere");
        std::fs::create_dir_all(&real).unwrap();
        let link = std::fs::canonicalize(tmp.path()).unwrap().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        policy.extra_read_roots.push(link.clone());

        let sbpl = SeatbeltLauncher::new()
            .unwrap()
            .profile(&python, &policy)
            .unwrap();
        assert!(sbpl.contains(&*real.to_string_lossy()));
        assert!(
            !sbpl.contains(&format!("\"{}\"", link.to_string_lossy())),
            "the profile must name the resolved path, not the symlink"
        );
    }

    #[test]
    fn a_path_that_could_break_out_of_a_rule_is_refused() {
        assert!(quote(Path::new("/tmp/ok")).is_ok());
        assert_eq!(quote(Path::new("/tmp/a\"b")).unwrap(), "\"/tmp/a\\\"b\"");
        assert!(quote(Path::new("/tmp/a\nb")).is_err());
    }

    #[test]
    fn a_missing_interpreter_is_a_launch_error_not_a_weaker_profile() {
        let (_tmp, _python, policy) = fixture();
        let err = SeatbeltLauncher::new()
            .unwrap()
            .profile(Path::new("/nope/python3"), &policy)
            .unwrap_err();
        assert!(err.contains("python interpreter"), "{err}");
    }
}
