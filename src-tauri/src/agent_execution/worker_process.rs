//! The host half of the worker protocol (contract C2, design §14.3/§16.2).
//!
//! One [`WorkerHandle`] owns one live CPython kernel: its pipes, its reader
//! threads, its process group, and the interruption ladder. It knows nothing
//! about threads, tracks, Tauri or SQLite — a workspace directory, an
//! interpreter, a script and a launcher is the whole world.
//!
//! ## Why this is synchronous
//!
//! A kernel executes exactly one cell at a time, so the "concurrency" here is
//! two pipe-reader threads and a deadline. Async buys nothing and costs a
//! runtime-flavoured API in a module that the headless harness and the tests
//! also drive. The async command layer wraps a call in `spawn_blocking`, which
//! is already the house pattern for the other Python workers.
//!
//! ## Frame routing
//!
//! The reader thread pushes every parsed frame onto one channel. Because a
//! kernel serves one request at a time, the consumer is whichever call holds
//! the receiver lock; frames carrying a *different* execution id belong to an
//! abandoned request (a late `interrupted` result, say) and are logged and
//! dropped. That id guard is what keeps a late cancellation from landing on the
//! next cell (§16.1).

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::agent_execution::worker_launcher::{SandboxPolicy, WorkerLauncher};

/// How long a cold worker gets to import the analysis stack and say `ready`.
pub const READY_TIMEOUT: Duration = Duration::from_secs(120);
/// Grace period between `SIGINT` and `SIGKILL` (design §16.2 step 2).
pub const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
/// How long a graceful `shutdown` may take before the group is killed.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Poll granularity for cancellation while waiting on frames.
const POLL: Duration = Duration::from_millis(25);
/// Bytes of raw child stderr retained for crash diagnosis.
const STDERR_TAIL_BYTES: usize = 8 * 1024;
/// A semantic binding should batch work rather than turn the worker protocol
/// into a chatty RPC bus. This also bounds the damage from a buggy loop.
const MAX_HOST_CALLS_PER_CELL: usize = 64;

// The two rungs of the ladder. Spelled out so the module compiles on platforms
// without a `libc` signal set; `signal_group` ignores them there.
const SIG_INT: i32 = 2;
const SIG_KILL: i32 = 9;

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// A one-shot cancellation flag. Cloned handles share one flag, so the model
/// turn's abort path can cancel a cell it does not own.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    state: Arc<AtomicU8>,
}

const CANCEL_REQUESTED: u8 = 1;
const IRREVERSIBLE: u8 = 2;

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.state.fetch_or(CANCEL_REQUESTED, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        let state = self.state.load(Ordering::SeqCst);
        state & CANCEL_REQUESTED != 0 && state & IRREVERSIBLE == 0
    }

    fn begin_irreversible(&self) -> bool {
        let mut observed = self.state.load(Ordering::SeqCst);
        loop {
            if observed & CANCEL_REQUESTED != 0 {
                return false;
            }
            match self.state.compare_exchange(
                observed,
                observed | IRREVERSIBLE,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => observed = actual,
            }
        }
    }

    fn end_irreversible(&self) {
        self.state.fetch_and(!IRREVERSIBLE, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Scoped host calls
// ---------------------------------------------------------------------------

/// A structured rejection returned to Python as `LumaHostCallError`.
///
/// Codes are deliberately strings: this transport is domain-agnostic, while a
/// scoped handler may need precise errors such as `conflict` or `invalid_clip`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCallError {
    pub code: String,
    pub message: String,
}

impl HostCallError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn unavailable() -> Self {
        Self::new(
            "unavailable",
            "host calls are not available for this Python cell",
        )
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new("internal", message)
    }
}

/// Supervision inherited from the cell that issued a host call. Domain
/// handlers must stop before this deadline and treat cancellation as a request
/// to abandon any uncommitted work.
#[derive(Clone)]
pub struct HostCallContext {
    cancel: CancelToken,
    deadline: Instant,
}

impl HostCallContext {
    fn new(cancel: CancelToken, deadline: Instant) -> Self {
        Self { cancel, deadline }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.deadline.checked_duration_since(Instant::now())
    }

    pub fn check(&self) -> Result<(), HostCallError> {
        if self.is_cancelled() {
            Err(HostCallError::new(
                "cancelled",
                "the cell was cancelled while the host call was running",
            ))
        } else if self.remaining().is_none() {
            Err(HostCallError::new(
                "timeout",
                "the cell deadline expired while the host call was running",
            ))
        } else {
            Ok(())
        }
    }

    /// Cross the point after which a host mutation must return its authoritative
    /// result. The transition is atomic with cancellation: either Stop wins and
    /// no write begins, or the write wins and cancellation is deferred until
    /// the correlated host response has been written back to Python.
    pub fn begin_irreversible(&self) -> Result<(), HostCallError> {
        self.check()?;
        if self.cancel.begin_irreversible() {
            Ok(())
        } else {
            Err(HostCallError::new(
                "cancelled",
                "the cell was cancelled before the track write began",
            ))
        }
    }

    fn end_irreversible(&self) {
        self.cancel.end_irreversible();
    }
}

/// One cell's capability table. The command layer constructs a handler from
/// trusted scope; Python supplies only a method name and JSON payload.
///
/// This trait is synchronous because [`WorkerHandle`] is synchronous. A domain
/// backed by async services may capture a Tokio handle and `block_on` from the
/// command layer's existing `spawn_blocking` task.
pub trait HostCallHandler: Send + Sync {
    fn handle(
        &self,
        method: &str,
        payload: Value,
        context: &HostCallContext,
    ) -> Result<Value, HostCallError>;
}

impl<F> HostCallHandler for F
where
    F: Fn(&str, Value, &HostCallContext) -> Result<Value, HostCallError> + Send + Sync,
{
    fn handle(
        &self,
        method: &str,
        payload: Value,
        context: &HostCallContext,
    ) -> Result<Value, HostCallError> {
        self(method, payload, context)
    }
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecStatus {
    Ok,
    Error,
    Interrupted,
    /// Infrastructure failure: the worker died, timed out past the ladder, or
    /// spoke nonsense. Distinct from a Python-level error.
    Failed {
        reason: String,
    },
}

impl ExecStatus {
    pub fn is_ok(&self) -> bool {
        matches!(self, ExecStatus::Ok)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FigureRef {
    pub artifact_rel: String,
    pub width: u32,
    pub height: u32,
}

/// What one `exec` produced. Mirrors the worker's terminal `result` frame plus
/// the host-side facts the worker cannot know (whether the namespace survived).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecOutcome {
    pub status: ExecStatus,
    pub stdout: String,
    pub stderr: String,
    pub repr: Option<String>,
    pub traceback: Option<String>,
    pub figures: Vec<FigureRef>,
    pub warnings: Vec<String>,
    pub truncated: Truncation,
    pub duration_ms: u64,
    /// The kernel namespace did not survive this execution — the process was
    /// killed or crashed. The caller must respawn and tell the agent (§13.4).
    pub state_lost: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Truncation {
    pub stdout: bool,
    pub stderr: bool,
    pub repr: bool,
}

impl ExecOutcome {
    pub(crate) fn interrupted_before_start(started: Instant) -> Self {
        Self {
            status: ExecStatus::Interrupted,
            stdout: String::new(),
            stderr: String::new(),
            repr: None,
            traceback: None,
            figures: Vec::new(),
            warnings: Vec::new(),
            truncated: Truncation::default(),
            duration_ms: started.elapsed().as_millis() as u64,
            state_lost: false,
        }
    }

    fn failed(reason: impl Into<String>, state_lost: bool, started: Instant) -> Self {
        Self {
            status: ExecStatus::Failed {
                reason: reason.into(),
            },
            stdout: String::new(),
            stderr: String::new(),
            repr: None,
            traceback: None,
            figures: Vec::new(),
            warnings: Vec::new(),
            truncated: Truncation::default(),
            duration_ms: started.elapsed().as_millis() as u64,
            state_lost,
        }
    }
}

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

/// Everything needed to start one kernel. No Tauri types: the headless harness
/// and the tests build this directly.
pub struct WorkerConfig {
    pub python_bin: PathBuf,
    pub worker_script: PathBuf,
    pub workspace_dir: PathBuf,
    pub launcher: Box<dyn WorkerLauncher>,
}

/// The worker's `ready` frame (contract C2 + appendix A.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadyInfo {
    pub pid: i32,
    pub python: String,
    pub warnings: Vec<String>,
    pub startup_ms: u64,
}

enum Event {
    Frame(Value),
    /// The protocol stream closed — the worker is gone.
    Eof,
}

pub struct WorkerHandle {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    events: Mutex<Receiver<Event>>,
    stderr_tail: Arc<Mutex<Tail>>,
    ready: ReadyInfo,
    pid: i32,
    alive: AtomicBool,
    counter: AtomicU64,
    launcher: &'static str,
}

impl WorkerHandle {
    /// Launch a worker and block until it reports `ready`.
    pub fn spawn(config: WorkerConfig) -> Result<Self, String> {
        let WorkerConfig {
            python_bin,
            worker_script,
            workspace_dir,
            launcher,
        } = config;

        let policy = SandboxPolicy::new(
            workspace_dir,
            worker_script
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            venv_root(&python_bin),
        );

        let mut child = launcher
            .launch(&python_bin, &worker_script, &policy)
            .map_err(|e| format!("failed to launch python worker ({}): {e}", launcher.name()))?;
        let pid = child.id() as i32;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "python worker has no stdout pipe".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "python worker has no stderr pipe".to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "python worker has no stdin pipe".to_string())?;

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name(format!("luma-exec-proto-{pid}"))
            .spawn(move || read_frames(stdout, tx))
            .map_err(|e| format!("failed to start protocol reader: {e}"))?;

        let tail = Arc::new(Mutex::new(Tail::new(STDERR_TAIL_BYTES)));
        {
            let tail = Arc::clone(&tail);
            std::thread::Builder::new()
                .name(format!("luma-exec-stderr-{pid}"))
                .spawn(move || drain_stderr(stderr, tail))
                .map_err(|e| format!("failed to start stderr drain: {e}"))?;
        }

        let ready = match await_ready(&rx, pid, &tail) {
            Ok(ready) => ready,
            Err(e) => {
                signal_pid_group(pid, SIG_KILL);
                let _ = child.wait();
                return Err(e);
            }
        };

        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            events: Mutex::new(rx),
            stderr_tail: tail,
            ready,
            pid,
            alive: AtomicBool::new(true),
            counter: AtomicU64::new(0),
            launcher: launcher.name(),
        })
    }

    pub fn ready(&self) -> &ReadyInfo {
        &self.ready
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn launcher_name(&self) -> &'static str {
        self.launcher
    }

    /// Last bytes the child wrote to its *raw* stderr — i.e. crash output the
    /// protocol never got to describe.
    pub fn stderr_tail(&self) -> String {
        self.stderr_tail.lock().unwrap().to_string()
    }

    pub fn is_alive(&self) -> bool {
        if !self.alive.load(Ordering::SeqCst) {
            return false;
        }
        match self.child.lock().unwrap().try_wait() {
            Ok(Some(_)) => {
                self.alive.store(false, Ordering::SeqCst);
                false
            }
            Ok(None) => true,
            Err(_) => false,
        }
    }

    /// Fresh execution id. Ids are unique per handle, which is what makes a
    /// late cancellation harmless.
    pub fn next_id(&self) -> String {
        format!("c-{}", self.counter.fetch_add(1, Ordering::SeqCst) + 1)
    }

    // -- operations ------------------------------------------------------

    /// Run one cell. Blocks until a terminal result, the deadline, or the end
    /// of the interruption ladder.
    pub fn exec(
        &self,
        id: &str,
        code: &str,
        manifest_rel: Option<&str>,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> ExecOutcome {
        self.exec_inner(id, code, manifest_rel, timeout, cancel, None)
    }

    /// Run one cell with a scoped synchronous capability handler.
    pub fn exec_with_host(
        &self,
        id: &str,
        code: &str,
        manifest_rel: Option<&str>,
        timeout: Duration,
        cancel: &CancelToken,
        host: &dyn HostCallHandler,
    ) -> ExecOutcome {
        self.exec_inner(id, code, manifest_rel, timeout, cancel, Some(host))
    }

    fn exec_inner(
        &self,
        id: &str,
        code: &str,
        manifest_rel: Option<&str>,
        timeout: Duration,
        cancel: &CancelToken,
        host: Option<&dyn HostCallHandler>,
    ) -> ExecOutcome {
        let started = Instant::now();
        if cancel.is_cancelled() {
            return ExecOutcome::interrupted_before_start(started);
        }
        let mut request = json!({ "id": id, "op": "exec", "code": code });
        if let Some(rel) = manifest_rel {
            request["manifest_rel"] = Value::String(rel.to_string());
        }
        if let Err(e) = self.send(&request) {
            self.mark_dead();
            return ExecOutcome::failed(e, true, started);
        }
        self.collect(id, started, timeout, cancel, host)
    }

    fn collect(
        &self,
        id: &str,
        started: Instant,
        timeout: Duration,
        cancel: &CancelToken,
        host: Option<&dyn HostCallHandler>,
    ) -> ExecOutcome {
        let events = self.events.lock().unwrap();
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut host_call_ids = HashSet::new();
        let mut host_call_count = 0usize;
        // SIGINT is meaningful only after the worker has entered its protected
        // execution region. A cancel may be requested after we write `exec` but
        // before Python reads it; signalling in that gap is deliberately ignored
        // by the worker and would otherwise leave the cell running forever.
        let mut worker_started = false;
        // Phase 1: normal wait. Phase 2 (after SIGINT): the grace window.
        let mut deadline = started + timeout;
        let mut escalation: Option<Escalation> = None;

        loop {
            if escalation.is_none() && cancel.is_cancelled() && worker_started {
                escalation = Some(Escalation::Cancelled);
                self.signal_group(SIG_INT);
                deadline = Instant::now() + INTERRUPT_GRACE;
            }

            match wait_event(&events, deadline) {
                Wait::Idle => continue,
                Wait::Event(Event::Frame(frame)) => {
                    let frame_id = frame.get("id").and_then(Value::as_str);
                    match frame.get("type").and_then(Value::as_str) {
                        Some("started") if frame_id == Some(id) => {
                            worker_started = true;
                        }
                        Some("stream") if frame_id == Some(id) => {
                            let text = string_field(&frame, "text").unwrap_or_default();
                            match frame.get("stream").and_then(Value::as_str) {
                                Some("stderr") => stderr.push_str(&text),
                                _ => stdout.push_str(&text),
                            }
                        }
                        Some("host_call") if frame_id == Some(id) => {
                            host_call_count += 1;
                            if let Err(e) = self.answer_host_call(
                                id,
                                &frame,
                                host,
                                cancel,
                                deadline,
                                &mut host_call_ids,
                                host_call_count,
                            ) {
                                self.mark_dead();
                                let mut outcome = ExecOutcome::failed(e, true, started);
                                outcome.stdout = stdout;
                                outcome.stderr = stderr;
                                return outcome;
                            }
                        }
                        Some("result") if frame_id == Some(id) => {
                            return self.finish(frame, stdout, stderr, started, escalation);
                        }
                        _ => log::warn!(
                            "[agent-exec] dropping frame for another request while running {id}: {frame}"
                        ),
                    }
                }
                Wait::Event(Event::Eof) => {
                    self.mark_dead();
                    let tail = self.stderr_tail();
                    let mut outcome = ExecOutcome::failed(
                        format!("python worker exited mid-cell: {tail}"),
                        true,
                        started,
                    );
                    outcome.stdout = stdout;
                    outcome.stderr = stderr;
                    return outcome;
                }
                Wait::Deadline => match escalation {
                    // Deadline reached with the kernel still running: SIGINT
                    // the group and give it the grace window (§16.2 1-3).
                    None => {
                        escalation = Some(Escalation::TimedOut(timeout));
                        self.signal_group(SIG_INT);
                        deadline = Instant::now() + INTERRUPT_GRACE;
                    }
                    // Grace window expired: kill the group, namespace is lost
                    // (§16.2 4-6).
                    Some(kind) => {
                        drop(events);
                        self.kill_group();
                        self.mark_dead();
                        let mut outcome = match kind {
                            Escalation::Cancelled => ExecOutcome {
                                status: ExecStatus::Interrupted,
                                stdout: String::new(),
                                stderr: String::new(),
                                repr: None,
                                traceback: None,
                                figures: Vec::new(),
                                warnings: vec![
                                    "the cell ignored the interrupt; the kernel was killed"
                                        .to_string(),
                                ],
                                truncated: Truncation::default(),
                                duration_ms: started.elapsed().as_millis() as u64,
                                state_lost: true,
                            },
                            Escalation::TimedOut(limit) => ExecOutcome::failed(
                                format!(
                                    "cell exceeded its {}s limit and ignored the interrupt; \
                                     the kernel was killed",
                                    limit.as_secs()
                                ),
                                true,
                                started,
                            ),
                        };
                        outcome.stdout = stdout;
                        outcome.stderr = stderr;
                        return outcome;
                    }
                },
            }
        }
    }

    /// Answer a worker-side capability request before waiting for more frames.
    /// The worker's main loop is paused inside the Python call and reads this
    /// response directly from stdin, so every request — including malformed or
    /// unavailable ones — must receive exactly one response.
    #[allow(clippy::too_many_arguments)]
    fn answer_host_call(
        &self,
        execution_id: &str,
        frame: &Value,
        host: Option<&dyn HostCallHandler>,
        cancel: &CancelToken,
        deadline: Instant,
        seen: &mut HashSet<String>,
        count: usize,
    ) -> Result<(), String> {
        let context = HostCallContext::new(cancel.clone(), deadline);
        let call_id = frame
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let method = frame.get("method").and_then(Value::as_str);

        let result = if call_id.is_empty() {
            Err(HostCallError::new(
                "invalid_request",
                "host call is missing a call_id",
            ))
        } else if !seen.insert(call_id.to_string()) {
            Err(HostCallError::new(
                "duplicate_call",
                format!("host call id '{call_id}' was already used in this cell"),
            ))
        } else if count > MAX_HOST_CALLS_PER_CELL {
            Err(HostCallError::new(
                "call_limit",
                format!("a cell may make at most {MAX_HOST_CALLS_PER_CELL} host calls"),
            ))
        } else if cancel.is_cancelled() {
            Err(HostCallError::new(
                "cancelled",
                "the cell was cancelled before the host call began",
            ))
        } else if method.is_none_or(str::is_empty) {
            Err(HostCallError::new(
                "invalid_request",
                "host call is missing a method",
            ))
        } else if let Some(handler) = host {
            let method = method.expect("checked above");
            let payload = frame.get("payload").cloned().unwrap_or(Value::Null);
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handler.handle(method, payload, &context)
            })) {
                Ok(result) => result,
                Err(_) => Err(HostCallError::internal("the host-call handler panicked")),
            }
        } else {
            Err(HostCallError::unavailable())
        };

        let response = match result {
            Ok(value) => json!({
                "id": execution_id,
                "op": "host_response",
                "call_id": call_id,
                "ok": true,
                "value": value,
            }),
            Err(error) => json!({
                "id": execution_id,
                "op": "host_response",
                "call_id": call_id,
                "ok": false,
                "error": {
                    "code": error.code,
                    "message": error.message,
                },
            }),
        };
        // A mutation may have crossed its commit barrier while the handler was
        // running. Keep Stop deferred only until this authoritative response is
        // flushed; any pending cancellation becomes visible immediately after.
        let sent = self.send(&response);
        context.end_irreversible();
        sent
    }

    fn finish(
        &self,
        frame: Value,
        stdout: String,
        stderr: String,
        started: Instant,
        escalation: Option<Escalation>,
    ) -> ExecOutcome {
        let status = match frame.get("status").and_then(Value::as_str) {
            Some("ok") => ExecStatus::Ok,
            Some("error") => ExecStatus::Error,
            Some("interrupted") => ExecStatus::Interrupted,
            other => ExecStatus::Failed {
                reason: format!("worker reported unknown status {other:?}"),
            },
        };
        // A cell that finished *after* we asked it to stop is still a stop as
        // far as the caller is concerned, but the namespace survived.
        let status = match (escalation, status) {
            (Some(_), ExecStatus::Ok | ExecStatus::Error) => ExecStatus::Interrupted,
            (_, s) => s,
        };
        let truncated = frame.get("truncated");
        ExecOutcome {
            status,
            stdout,
            stderr,
            repr: string_field(&frame, "repr"),
            traceback: string_field(&frame, "traceback"),
            figures: frame
                .get("figures")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(figure_ref).collect())
                .unwrap_or_default(),
            warnings: string_list(&frame, "warnings"),
            truncated: Truncation {
                stdout: flag(truncated, "stdout"),
                stderr: flag(truncated, "stderr"),
                repr: flag(truncated, "repr"),
            },
            duration_ms: frame
                .get("duration_ms")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| started.elapsed().as_millis() as u64),
            state_lost: false,
        }
    }

    /// Round-trip a `ping`. Returns the worker's reported pid.
    pub fn ping(&self, timeout: Duration) -> Result<i32, String> {
        let id = self.next_id();
        self.send(&json!({ "id": id, "op": "ping" }))?;
        let deadline = Instant::now() + timeout;
        let events = self.events.lock().unwrap();
        loop {
            match wait_event(&events, deadline) {
                Wait::Idle => continue,
                Wait::Event(Event::Frame(frame)) => {
                    if frame.get("id").and_then(Value::as_str) == Some(id.as_str())
                        && frame.get("type").and_then(Value::as_str) == Some("pong")
                    {
                        return Ok(
                            frame.get("pid").and_then(Value::as_i64).unwrap_or_default() as i32
                        );
                    }
                    log::warn!("[agent-exec] dropping frame while pinging: {frame}");
                }
                Wait::Event(Event::Eof) => {
                    self.mark_dead();
                    return Err(format!("python worker exited: {}", self.stderr_tail()));
                }
                Wait::Deadline => return Err("python worker did not answer ping".to_string()),
            }
        }
    }

    /// Ask the worker to exit, then make sure it did.
    pub fn shutdown(&self) {
        if self.alive.load(Ordering::SeqCst) {
            let id = self.next_id();
            let _ = self.send(&json!({ "id": id, "op": "shutdown" }));
            if let Ok(events) = self.events.lock() {
                let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
                loop {
                    match wait_event(&events, deadline) {
                        Wait::Idle => continue,
                        Wait::Event(Event::Frame(frame))
                            if frame.get("type").and_then(Value::as_str) == Some("goodbye") =>
                        {
                            break
                        }
                        Wait::Event(Event::Frame(_)) => {}
                        Wait::Event(Event::Eof) | Wait::Deadline => break,
                    }
                }
            }
            let _ = self.wait_for_exit(SHUTDOWN_TIMEOUT);
        }
        self.kill_group();
        self.mark_dead();
    }

    // -- plumbing --------------------------------------------------------

    fn send(&self, request: &Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().unwrap();
        let line = format!("{request}\n");
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("failed to write to python worker: {e}"))
    }

    fn mark_dead(&self) {
        self.alive.store(false, Ordering::SeqCst);
    }

    fn signal_group(&self, signal: i32) {
        #[cfg(unix)]
        signal_pid_group(self.pid, signal);
        #[cfg(not(unix))]
        {
            let _ = signal;
            let _ = self.child.lock().unwrap().kill();
        }
    }

    /// SIGKILL the whole group and reap the child so no zombie is left.
    fn kill_group(&self) {
        self.signal_group(SIG_KILL);
        if self.wait_for_exit(Duration::from_secs(2)).is_none() {
            let mut child = self.child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn wait_for_exit(&self, timeout: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.lock().unwrap().try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => {}
                Err(_) => return None,
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        if self.alive.load(Ordering::SeqCst) {
            self.signal_group(SIG_KILL);
            let _ = self.child.lock().unwrap().wait();
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Escalation {
    Cancelled,
    TimedOut(Duration),
}

// ---------------------------------------------------------------------------
// Reader threads
// ---------------------------------------------------------------------------

/// Block until the worker announces itself, or until the cold-start budget runs
/// out. Runs before the handle exists, so it takes the pieces directly.
fn await_ready(
    events: &Receiver<Event>,
    pid: i32,
    tail: &Arc<Mutex<Tail>>,
) -> Result<ReadyInfo, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match wait_event(events, deadline) {
            Wait::Idle => continue,
            Wait::Event(Event::Frame(frame)) => match frame.get("type").and_then(Value::as_str) {
                Some("ready") => {
                    return Ok(ReadyInfo {
                        pid: frame
                            .get("pid")
                            .and_then(Value::as_i64)
                            .unwrap_or(pid as i64) as i32,
                        python: string_field(&frame, "python").unwrap_or_default(),
                        warnings: string_list(&frame, "warnings"),
                        startup_ms: frame
                            .get("startup_ms")
                            .and_then(Value::as_u64)
                            .unwrap_or_default(),
                    })
                }
                _ => log::warn!("[agent-exec] frame before ready: {frame}"),
            },
            Wait::Event(Event::Eof) => {
                return Err(format!(
                    "python worker exited during startup: {}",
                    tail.lock().unwrap()
                ))
            }
            Wait::Deadline => {
                return Err(format!(
                    "python worker did not become ready within {}s: {}",
                    READY_TIMEOUT.as_secs(),
                    tail.lock().unwrap()
                ))
            }
        }
    }
}

#[cfg(unix)]
fn signal_pid_group(pid: i32, signal: i32) {
    // SAFETY: `killpg` on a pid we spawned into its own group; the worst case
    // for a reaped pid is ESRCH, which we log.
    unsafe {
        if libc::killpg(pid, signal) != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                log::warn!("[agent-exec] killpg({pid}, {signal}) failed: {err}");
            }
        }
    }
}

#[cfg(not(unix))]
fn signal_pid_group(_pid: i32, _signal: i32) {}

fn read_frames(stdout: std::process::ChildStdout, tx: Sender<Event>) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            // A worker-side `{"id":null,"type":"error"}` (malformed request) is
            // forwarded like any other frame; the id guard drops it.
            Ok(frame) => {
                if tx.send(Event::Frame(frame)).is_err() {
                    return;
                }
            }
            Err(e) => log::warn!("[agent-exec] unparseable protocol line ({e}): {line}"),
        }
    }
    let _ = tx.send(Event::Eof);
}

fn drain_stderr(mut stderr: std::process::ChildStderr, tail: Arc<Mutex<Tail>>) {
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => tail.lock().unwrap().push(&buf[..n]),
        }
    }
}

/// A bounded ring of the most recent bytes.
struct Tail {
    limit: usize,
    bytes: VecDeque<u8>,
}

impl Tail {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            bytes: VecDeque::new(),
        }
    }

    fn push(&mut self, data: &[u8]) {
        self.bytes.extend(data.iter().copied());
        while self.bytes.len() > self.limit {
            self.bytes.pop_front();
        }
    }
}

impl std::fmt::Display for Tail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bytes: Vec<u8> = self.bytes.iter().copied().collect();
        f.write_str(String::from_utf8_lossy(&bytes).trim())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One `POLL`-sized step of waiting. The caller re-checks cancellation between
/// steps — waiting the whole way to the deadline in here would make a cancel
/// arrive no sooner than a timeout.
enum Wait {
    Event(Event),
    /// Nothing yet; the deadline has not passed.
    Idle,
    Deadline,
}

fn wait_event(events: &Receiver<Event>, deadline: Instant) -> Wait {
    let now = Instant::now();
    if now >= deadline {
        return Wait::Deadline;
    }
    match events.recv_timeout(POLL.min(deadline - now)) {
        Ok(event) => Wait::Event(event),
        Err(RecvTimeoutError::Timeout) => Wait::Idle,
        Err(RecvTimeoutError::Disconnected) => Wait::Event(Event::Eof),
    }
}

fn string_field(frame: &Value, key: &str) -> Option<String> {
    frame.get(key).and_then(Value::as_str).map(str::to_string)
}

fn string_list(frame: &Value, key: &str) -> Vec<String> {
    frame
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn flag(truncated: Option<&Value>, key: &str) -> bool {
    truncated
        .and_then(|t| t.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn figure_ref(value: &Value) -> Option<FigureRef> {
    Some(FigureRef {
        artifact_rel: value.get("artifact_rel")?.as_str()?.to_string(),
        width: value.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
        height: value.get("height").and_then(Value::as_u64).unwrap_or(0) as u32,
    })
}

/// `<venv>/bin/python3` → `<venv>`.
fn venv_root(python_bin: &Path) -> PathBuf {
    python_bin
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn venv_root_strips_bin_python() {
        assert_eq!(
            venv_root(Path::new("/cache/python-env/bin/python3")),
            Path::new("/cache/python-env")
        );
    }

    #[test]
    fn cancel_token_is_shared_across_clones() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[test]
    fn host_call_context_inherits_cancel_and_deadline() {
        let token = CancelToken::new();
        let active = HostCallContext::new(token.clone(), Instant::now() + Duration::from_secs(1));
        assert!(active.check().is_ok());
        token.cancel();
        assert_eq!(active.check().unwrap_err().code, "cancelled");

        let expired = HostCallContext::new(
            CancelToken::new(),
            Instant::now() - Duration::from_millis(1),
        );
        assert_eq!(expired.check().unwrap_err().code, "timeout");
    }

    #[test]
    fn cancellation_wins_before_the_irreversible_barrier() {
        let token = CancelToken::new();
        let context = HostCallContext::new(token.clone(), Instant::now() + Duration::from_secs(1));
        token.cancel();

        assert_eq!(context.begin_irreversible().unwrap_err().code, "cancelled");
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancellation_is_deferred_only_while_irreversible() {
        let token = CancelToken::new();
        let context = HostCallContext::new(token.clone(), Instant::now() + Duration::from_secs(1));
        context.begin_irreversible().unwrap();

        token.cancel();
        assert!(!token.is_cancelled());
        context.end_irreversible();
        assert!(token.is_cancelled());
    }

    #[test]
    fn tail_keeps_the_last_bytes() {
        let mut tail = Tail::new(4);
        tail.push(b"abcdef");
        assert_eq!(tail.to_string(), "cdef");
    }
}
