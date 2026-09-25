//! Registering a file, and running each of its tests in an app of its own.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use gpui_agent::fixture::Fixture;
use gpui_agent::Mode;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::discover;
use crate::Options;

/// A test gets this long unless it says otherwise.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// On top of the test's own timeout: seeding the library and opening the app
/// happen before the script's clock starts. A test past both is abandoned.
const GRACE: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Skip,
}

pub struct Outcome {
    pub file: String,
    pub test: String,
    pub status: Status,
    pub duration: Duration,
    /// Seeding the library and opening the app, before the script ran.
    pub setup: Duration,
    pub error: Option<String>,
    /// `path:line  source` of the failing line in the test file.
    pub at: Option<String>,
    /// The last frame's `role:label`s.
    pub frame: Option<Vec<String>>,
    pub shot: Option<String>,
    pub console: String,
    /// A failed test's seeded library, left on disk.
    pub library: Option<String>,
}

impl Outcome {
    fn new(file: &str, test: &str, status: Status) -> Self {
        Self {
            file: file.to_string(),
            test: test.to_string(),
            status,
            duration: Duration::ZERO,
            setup: Duration::ZERO,
            error: None,
            at: None,
            frame: None,
            shot: None,
            console: String::new(),
            library: None,
        }
    }

    fn failed(file: &str, test: &str, error: impl Into<String>) -> Self {
        Self {
            error: Some(error.into()),
            ..Self::new(file, test, Status::Fail)
        }
    }
}

pub struct Job {
    label: String,
    /// The file's path as stack traces spell it.
    display: String,
    path: PathBuf,
    test: String,
    fixture: Value,
    pixel: bool,
    timeout: Duration,
}

// -- helpers ---------------------------------------------------------------

/// Read from the source tree when it is there, so editing a helper needs no
/// rebuild; the copy compiled in is for a binary run elsewhere.
fn helper(path: &str, embedded: &'static str) -> String {
    std::fs::read_to_string(crate_dir().join(path)).unwrap_or_else(|_| embedded.to_string())
}

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

pub fn helper_paths() -> Vec<PathBuf> {
    HELPERS
        .iter()
        .map(|(path, _)| crate_dir().join(path))
        .collect()
}

/// Loaded in this order into every test's interpreter, after the prelude.
/// `until` and `nav` live beside the cargo suites' support code, which
/// splices the same files.
const HELPERS: [(&str, &str); 3] = [
    ("src/runner.js", include_str!("../../runner.js")),
    (
        "tests/support/until.js",
        include_str!("../../../tests/support/until.js"),
    ),
    (
        "tests/support/nav.js",
        include_str!("../../../tests/support/nav.js"),
    ),
];

fn display(path: &Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    absolute
        .strip_prefix(&cwd)
        .unwrap_or(&absolute)
        .display()
        .to_string()
}

fn label(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.trim_end_matches(".test.js").to_string()
}

// -- registration ------------------------------------------------------------

#[derive(Deserialize)]
struct Registry {
    fixture: Option<Map<String, Value>>,
    tests: Vec<Registered>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registered {
    name: String,
    skip: bool,
    fixture: Option<Map<String, Value>>,
    timeout_ms: Option<u64>,
    pixel: bool,
}

/// Load `path` with no app, to learn its fixture and tests. Tests this run
/// will not execute (skipped, filtered out, other mode) come back as
/// outcomes or not at all; the rest come back as jobs.
pub fn register(path: &Path, options: &Options) -> Result<(Vec<Job>, Vec<Outcome>), Outcome> {
    let label = label(path);
    let display = display(path);
    let load_error = |error: String| Outcome::failed(&label, "(load)", error);
    let source = std::fs::read_to_string(path)
        .map_err(|error| load_error(format!("could not read {display}: {error}")))?;
    let registry = evaluate(&source, &display).map_err(load_error)?;
    let registry: Registry = serde_json::from_str(&registry)
        .map_err(|error| load_error(format!("bad registry: {error}")))?;

    let mut file_fixture = registry.fixture.unwrap_or_default();
    let file_pixel = match file_fixture.remove("mode") {
        None => discover::under_pixel(path),
        Some(Value::String(mode)) if mode == "pixel" => true,
        Some(Value::String(mode)) if mode == "headless" => false,
        Some(other) => {
            return Err(load_error(format!(
                "fixture mode must be \"headless\" or \"pixel\", got {other}"
            )))
        }
    };

    let (mut jobs, mut skipped) = (Vec::new(), Vec::new());
    for test in registry.tests {
        if let Some(filter) = &options.filter {
            if !format!("{label} › {}", test.name).contains(filter.as_str()) {
                continue;
            }
        }
        let pixel = file_pixel || test.pixel;
        if pixel != options.pixel {
            continue;
        }
        if test.skip {
            skipped.push(Outcome::new(&label, &test.name, Status::Skip));
            continue;
        }
        // A test's own fixture is laid over the file's, key by key.
        let mut fixture = file_fixture.clone();
        fixture.extend(test.fixture.unwrap_or_default());
        jobs.push(Job {
            label: label.clone(),
            display: display.clone(),
            path: path.to_path_buf(),
            test: test.name,
            fixture: Value::Object(fixture),
            pixel,
            timeout: test
                .timeout_ms
                .map_or(DEFAULT_TIMEOUT, Duration::from_millis),
        });
    }
    Ok((jobs, skipped))
}

/// Run `source` in a context of its own with the runner's vocabulary and no
/// app, and hand back the registry as JSON.
fn evaluate(source: &str, display: &str) -> Result<String, String> {
    use rquickjs::{context::EvalOptions, CatchResultExt, CaughtError, Context, Runtime};

    let runtime = Runtime::new().map_err(|error| error.to_string())?;
    // Registration runs top-level code only; anything slow there is a loop.
    let deadline = Instant::now() + Duration::from_secs(5);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let context = Context::full(&runtime).map_err(|error| error.to_string())?;
    let runner = helper(HELPERS[0].0, HELPERS[0].1);
    context.with(|ctx| {
        let run = |code: &str, name: &str| -> Result<String, String> {
            let mut options = EvalOptions::default();
            options.filename = Some(name.to_string());
            ctx.eval_with_options::<rquickjs::Value, _>(code, options)
                .catch(&ctx)
                .map(|value| {
                    value
                        .as_string()
                        .and_then(|text| text.to_string().ok())
                        .unwrap_or_default()
                })
                .map_err(|error| match error {
                    CaughtError::Exception(exception) => {
                        let message = exception.message().unwrap_or_default();
                        let stack = exception.stack().unwrap_or_default();
                        format!("{message}\n{}", stack.trim())
                    }
                    other => other.to_string(),
                })
        };
        run(
            "globalThis.console = { log() {}, info() {}, warn() {}, error() {} };",
            "luma-test",
        )?;
        run(&runner, "runner.js")?;
        run(source, display).map_err(|error| {
            if error.contains("is not defined") && error.contains("app") {
                format!("{error}\n(top-level code runs with no app; drive it inside test())")
            } else {
                error
            }
        })?;
        run("JSON.stringify(__registry)", "luma-test")
    })
}

// -- running -----------------------------------------------------------------

/// Run every job on `workers` threads, handing each outcome to `record` on
/// this thread as it lands.
pub fn all(jobs: Vec<Job>, workers: usize, mut record: impl FnMut(Outcome)) {
    let total = jobs.len();
    let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
    let (tx, rx) = mpsc::channel();
    for _ in 0..workers.min(total) {
        let queue = queue.clone();
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            let Some(job) = queue.lock().unwrap().pop_front() else {
                return;
            };
            if tx.send(guarded(job)).is_err() {
                return;
            }
        });
    }
    drop(tx);
    for outcome in rx {
        record(outcome);
    }
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// One test on a thread of its own, abandoned if it outlives its budget: a
/// wedged app cannot be interrupted, and it must not take the run with it.
fn guarded(job: Job) -> Outcome {
    let started = Instant::now();
    let since = SystemTime::now();
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let thread_name = format!("luma-test-{id}");
    let (label, test, budget) = (job.label.clone(), job.test.clone(), job.timeout + GRACE);
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || {
            let _ = tx.send(one(job, id));
        })
        .expect("failed to spawn a test thread");
    let mut outcome = match rx.recv_timeout(budget) {
        Ok(outcome) => outcome,
        Err(RecvTimeoutError::Timeout) => Outcome::failed(
            &label,
            &test,
            format!("hung for {budget:?} and was abandoned (the app thread is stuck)"),
        ),
        Err(RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            let panic = take_panic(&thread_name).unwrap_or_else(|| "the test thread died".into());
            Outcome::failed(&label, &test, format!("panicked: {panic}"))
        }
    };
    outcome.duration = started.elapsed();
    if outcome.status == Status::Fail {
        if let Some(panic) = pump_panic_since(since) {
            outcome
                .console
                .push_str(&format!("\n[an app thread panicked] {panic}"));
        }
    }
    outcome
}

fn one(job: Job, id: usize) -> Outcome {
    let started = Instant::now();
    let fail = |error: String| Outcome::failed(&job.label, &job.test, error);
    let name = format!(
        "lt{id}-{}",
        job.label.replace(|c: char| !c.is_ascii_alphanumeric(), "-")
    );
    let library = gpui_agent::fixture::config_dir(&name);
    let fixture = match Fixture::from_json(name, job.fixture.clone()) {
        Ok(fixture) => fixture,
        Err(error) => return fail(format!("bad fixture: {error}")),
    };
    let source = match std::fs::read_to_string(&job.path) {
        Ok(source) => source,
        Err(error) => return fail(format!("could not read {}: {error}", job.display)),
    };
    let mode = if job.pixel {
        #[cfg(feature = "pixel")]
        {
            Mode::Pixel
        }
        #[cfg(not(feature = "pixel"))]
        unreachable!("main refuses --pixel in a headless build")
    } else {
        Mode::Headless
    };
    let mut harness = match fixture.open_with(mode, job.timeout) {
        Ok(harness) => harness,
        Err(error) => return fail(format!("the app did not open: {error}")),
    };

    let only = json!(job.test).to_string();
    let setup = harness.exec(
        &format!("globalThis.__only = {only};"),
        Duration::from_secs(5),
    );
    if let Some(error) = setup.error {
        return fail(error);
    }
    for (path, embedded) in HELPERS {
        let name = path.rsplit('/').next().unwrap_or(path);
        let loaded = harness.exec_named(&helper(path, embedded), name, Duration::from_secs(5));
        if let Some(error) = loaded.error {
            return fail(format!("{name} failed to load: {error}"));
        }
    }

    let setup = started.elapsed();
    let result = harness.exec_named(&source, &job.display, job.timeout);
    let mut outcome = Outcome::new(&job.label, &job.test, Status::Pass);
    outcome.setup = setup;
    outcome.console = result.stdout;
    match result.error {
        None => {
            let ran = harness.exec("__ran", Duration::from_secs(5)).result;
            if ran != Value::Bool(true) {
                outcome.status = Status::Fail;
                outcome.error = Some(format!(
                    "no test named {only} ran (is its test() call conditional?)"
                ));
            }
        }
        Some(error) => {
            outcome.status = Status::Fail;
            outcome.at = locate(&error, &job.display, &source);
            outcome.error = Some(error);
            let seen = harness.exec(
                "({ frame: globalThis.__lastFrame?.nodes.map((n) => `${n.role}:${n.label}`) ?? null,
                    shot: globalThis.__lastShot ?? null })",
                Duration::from_secs(5),
            );
            outcome.frame = serde_json::from_value(seen.result["frame"].clone()).ok();
            outcome.shot = seen.result["shot"].as_str().map(str::to_string);
        }
    }
    // gpui has a headless renderer only on macOS at the pinned rev. A shot
    // this platform cannot take says nothing about the app.
    if outcome
        .error
        .as_deref()
        .is_some_and(|error| error.contains("no HeadlessRenderer configured"))
    {
        outcome.status = Status::Skip;
        outcome.error =
            Some("app.screenshot() needs a headless renderer, and this platform has none".into());
    }
    // A passing test's library is of no further use; a failing one's is
    // evidence. The app lets go of it as its thread winds down, and removing
    // a file SQLite still has open is harmless on Unix.
    drop(harness);
    if outcome.status == Status::Pass {
        std::fs::remove_dir_all(&library).ok();
    } else {
        outcome.library = Some(library.display().to_string());
    }
    outcome
}

/// The first stack frame in the test file, with that line's source.
fn locate(error: &str, display: &str, source: &str) -> Option<String> {
    let marker = format!("{display}:");
    let start = error.find(&marker)? + marker.len();
    let line: usize = error[start..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    let code = source
        .lines()
        .nth(line.checked_sub(1)?)
        .unwrap_or("")
        .trim();
    Some(format!("{display}:{line}  {code}"))
}

// -- panics ------------------------------------------------------------------

struct Panic {
    thread: String,
    message: String,
    at: SystemTime,
}

static PANICS: Mutex<Vec<Panic>> = Mutex::new(Vec::new());

/// Keep panics for the report instead of printing them over the results.
pub fn quiet_panics() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let location = info
            .location()
            .map(|at| format!(" ({}:{})", at.file(), at.line()))
            .unwrap_or_default();
        if let Ok(mut panics) = PANICS.lock() {
            panics.push(Panic {
                thread,
                message: format!("{message}{location}"),
                at: SystemTime::now(),
            });
        }
    }));
}

fn take_panic(thread: &str) -> Option<String> {
    let mut panics = PANICS.lock().ok()?;
    let index = panics.iter().position(|panic| panic.thread == thread)?;
    Some(panics.remove(index).message)
}

/// An app thread's panic surfaces in the test only as `PumpGone`; say why.
/// Tests run side by side, so this is "one did", not "yours did".
fn pump_panic_since(since: SystemTime) -> Option<String> {
    let panics = PANICS.lock().ok()?;
    panics
        .iter()
        .rev()
        .find(|panic| panic.thread == "gpui-agent-pump" && panic.at >= since)
        .map(|panic| panic.message.clone())
}
