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
        let mut child = command
            .spawn()
            .map_err(|e| protocol(format!("could not start agent: {e}")))?;
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

pub(super) fn command(name: &str, cwd: &std::path::Path) -> Command {
    let variable = format!("LUMA_{}_EXECUTABLE", name.to_uppercase());
    let executable = std::env::var_os(variable)
        .map(std::path::PathBuf::from)
        .or_else(|| {
            dirs::home_dir()
                .map(|home| home.join(".local/bin").join(name))
                .filter(|path| path.is_file())
        })
        .unwrap_or_else(|| name.into());
    let mut command = Command::new(executable);
    command.current_dir(cwd);
    command
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
