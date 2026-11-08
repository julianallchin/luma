//! Owned JSON-lines subprocess. Dropping it closes the pipe and terminates the
//! process group on Unix. The provider process has the same lifetime as its turn.

use super::{protocol, AgentError};
use serde_json::Value;
use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub(super) struct Process {
    child: Option<Child>,
    stdin: Input,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    reader: tokio::task::JoinHandle<()>,
}

/// A separate writer lets a session keep its protocol read future alive while
/// tool completions send replies. Never cancel a protocol handshake mid-write.
#[derive(Clone)]
pub(super) struct Input(Arc<tokio::sync::Mutex<ChildStdin>>);

impl Input {
    pub async fn send(&self, value: Value) -> Result<(), AgentError> {
        let mut bytes = serde_json::to_vec(&value).map_err(|e| protocol(e.to_string()))?;
        bytes.push(b'\n');
        self.0
            .lock()
            .await
            .write_all(&bytes)
            .await
            .map_err(|e| protocol(format!("agent stdin: {e}")))
    }
}

impl Process {
    pub fn start(mut command: Command) -> Result<Self, AgentError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|e| {
            protocol(format!(
                "could not start agent {}: {e}",
                command.as_std().get_program().to_string_lossy()
            ))
        })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut errors = BufReader::new(child.stderr.take().expect("piped stderr")).lines();
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let tail = Arc::clone(&stderr);
        let reader = tokio::spawn(async move {
            while let Ok(Some(line)) = errors.next_line().await {
                let mut tail = tail.lock().expect("stderr tail");
                if tail.len() == 8 {
                    tail.pop_front();
                }
                tail.push_back(line.chars().take(500).collect::<String>());
            }
        });
        Ok(Self {
            child: Some(child),
            stdin: Input(Arc::new(tokio::sync::Mutex::new(stdin))),
            lines: BufReader::new(stdout).lines(),
            stderr,
            reader,
        })
    }

    pub fn input(&self) -> Input {
        self.stdin.clone()
    }

    pub async fn send(&mut self, value: Value) -> Result<(), AgentError> {
        self.stdin.send(value).await
    }

    pub async fn read(&mut self) -> Result<Value, AgentError> {
        loop {
            match self.lines.next_line().await {
                Ok(Some(line)) if line.trim().is_empty() => continue,
                Ok(Some(line)) => {
                    return serde_json::from_str(&line)
                        .map_err(|e| protocol(format!("invalid agent frame: {e}")))
                }
                result => {
                    let tail = self
                        .stderr
                        .lock()
                        .expect("stderr tail")
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Err(protocol(format!(
                        "agent stream ended before completion ({result:?}): {tail}"
                    )));
                }
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            // SAFETY: this unreaped child is the leader of the group we created.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = child.start_kill();
        self.reader.abort();
        // Child::drop delegates reaping to Tokio's orphan queue, which may not
        // run again (especially during runtime shutdown). Own that final wait
        // outside the runtime. Only process cleanup survives the turn's drop.
        if matches!(child.try_wait(), Ok(None)) {
            if let Err(error) = std::thread::Builder::new()
                .name("agent-process-reaper".into())
                .spawn(move || loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                        Err(error) => {
                            eprintln!("[agent] could not reap native process: {error}");
                            break;
                        }
                    }
                })
            {
                eprintln!("[agent] could not start native process reaper: {error}");
            }
        }
    }
}

/// A user-installed agent CLI.
#[derive(Clone, Copy)]
pub(super) struct Cli {
    /// The executable's name: `codex`.
    pub name: &'static str,
    /// What a person calls it: `Codex CLI`.
    pub label: &'static str,
}

/// A command for `cli`, run in `cwd`: `LUMA_<NAME>_EXECUTABLE` when set,
/// otherwise the first `cli.name` on the user's PATH as their login shell
/// sees it (see [`crate::runtime::user_path`]). The child runs with that PATH
/// too, so the interpreter its `#!/usr/bin/env node` names is found the same
/// way, and with the shell-owned variables this process lacks.
///
/// # Errors
///
/// When the CLI is on none of those directories, naming it and where Luma
/// looked.
pub(super) fn command(cli: Cli, cwd: &std::path::Path) -> Result<Command, AgentError> {
    use crate::runtime::user_path;
    command_in(
        cli,
        cwd,
        std::env::var_os(override_variable(cli)),
        user_path::search_path(),
        user_path::child_env_defaults(),
    )
}

fn override_variable(cli: Cli) -> String {
    format!("LUMA_{}_EXECUTABLE", cli.name.to_uppercase())
}

fn command_in(
    cli: Cli,
    cwd: &std::path::Path,
    executable: Option<std::ffi::OsString>,
    search_path: &std::ffi::OsStr,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
) -> Result<Command, AgentError> {
    let executable = match executable.filter(|path| !path.is_empty()) {
        Some(path) => std::path::PathBuf::from(path),
        None => match crate::runtime::user_path::find_in(cli.name, search_path) {
            Some(path) => path,
            // Windows resolves a bare name itself, as it always has.
            None if cfg!(windows) => cli.name.into(),
            None => {
                return Err(protocol(format!(
                    "{} (`{}`) not found on your PATH. Install it or set {}. Luma looked in: {}",
                    cli.label,
                    cli.name,
                    override_variable(cli),
                    search_path.to_string_lossy()
                )))
            }
        },
    };
    eprintln!("[agent] {} is {}", cli.label, executable.display());
    let mut command = crate::runtime::command::async_command(&executable);
    command.current_dir(cwd);
    #[cfg(unix)]
    command.env("PATH", search_path);
    command.envs(env);
    Ok(command)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn dropping_after_runtime_shutdown_reaps_the_process() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (process, pid) = runtime.block_on(async {
            let mut command = tokio::process::Command::new("python3");
            command.args(["-c", "import os,json,time; print(json.dumps({'pid':os.getpid()}),flush=True); time.sleep(60)"]);
            let mut process = Process::start(command).unwrap();
            let pid = process.read().await.unwrap()["pid"].as_i64().unwrap() as i32;
            (process, pid)
        });
        drop(runtime);
        drop(process);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // SAFETY: signal zero only probes this known child PID.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "native process remained unreaped after runtime shutdown"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    const CODEX: Cli = Cli {
        name: "codex",
        label: "Codex CLI",
    };

    fn executable(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A Finder launch: this process has only the system PATH, and `codex`
    /// is a bun-installed script whose `#!/usr/bin/env` interpreter lives,
    /// like it, only on the PATH the user's login shell builds.
    #[tokio::test]
    async fn a_cli_only_the_login_shell_can_see_starts() {
        let dir = tempfile::tempdir().unwrap();
        let login_bin = dir.path().join("login-bin");
        std::fs::create_dir(&login_bin).unwrap();
        executable(
            &login_bin.join("fake-node"),
            "#!/bin/sh\nprintf '{\"script\":\"%s\"}\\n' \"$1\"\n",
        );
        executable(&login_bin.join("codex"), "#!/usr/bin/env fake-node\n");
        let shell = dir.path().join("shell");
        executable(
            &shell,
            &format!(
                "#!/bin/sh\necho 'rc noise'\nPATH=\"{}:/usr/bin:/bin\"; export PATH\n\
                 while [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"-c\" ]; then shift; exec /bin/sh -c \"$1\"; fi\n  shift\ndone\nexit 1\n",
                login_bin.display()
            ),
        );
        let bare = std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin");
        let search = crate::runtime::user_path::search_path_for(&shell, bare);

        let command = command_in(CODEX, dir.path(), None, &search, Vec::new()).unwrap();
        assert_eq!(
            std::path::Path::new(command.as_std().get_program()),
            login_bin.join("codex")
        );
        let mut process = Process::start(command).unwrap();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), process.read())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame["script"], login_bin.join("codex").to_str().unwrap());
    }

    #[test]
    fn a_missing_cli_is_named_with_where_luma_looked() {
        let dir = tempfile::tempdir().unwrap();
        let search = std::env::join_paths([dir.path().join("a"), dir.path().join("b")]).unwrap();
        let error = command_in(CODEX, dir.path(), None, &search, Vec::new())
            .err()
            .expect("no codex anywhere")
            .to_string();
        assert!(
            error.contains(
                "Codex CLI (`codex`) not found on your PATH. Install it or set LUMA_CODEX_EXECUTABLE."
            ),
            "{error}"
        );
        assert!(error.contains(search.to_str().unwrap()), "{error}");
    }

    #[test]
    fn the_executable_override_comes_first() {
        let dir = tempfile::tempdir().unwrap();
        let pinned = dir.path().join("pinned-codex");
        executable(&pinned, "#!/bin/sh\n");
        let found = dir.path().join("bin");
        std::fs::create_dir(&found).unwrap();
        executable(&found.join("codex"), "#!/bin/sh\n");
        let command = command_in(
            CODEX,
            dir.path(),
            Some(pinned.clone().into()),
            found.as_os_str(),
            vec![("SSL_CERT_FILE".into(), "/etc/corp.pem".into())],
        )
        .unwrap();
        assert_eq!(std::path::Path::new(command.as_std().get_program()), pinned);
        let envs: Vec<_> = command.as_std().get_envs().collect();
        assert!(envs.contains(&(std::ffi::OsStr::new("PATH"), Some(found.as_os_str()))));
        assert!(envs.contains(&(
            std::ffi::OsStr::new("SSL_CERT_FILE"),
            Some(std::ffi::OsStr::new("/etc/corp.pem"))
        )));
    }

    #[tokio::test]
    async fn dropping_the_session_stops_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("survived");
        let script = r#"
import subprocess,sys
subprocess.run([sys.executable,'-c',
    'import json,os,time,pathlib,sys; print(json.dumps({"pid":os.getpid()}),flush=True); time.sleep(0.25); pathlib.Path(sys.argv[1]).touch()',
    sys.argv[1]])
"#;
        let mut command = Command::new("python3");
        command.args(["-c", script]).arg(&marker);
        let mut process = Process::start(command).unwrap();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), process.read())
            .await
            .unwrap()
            .unwrap();
        assert!(frame["pid"].is_number());
        drop(process);
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(!marker.exists(), "descendant survived its session");
    }
}
