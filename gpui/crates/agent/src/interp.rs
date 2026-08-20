//! The interpreter thread: QuickJS, a persistent scratchpad, and a deadline.
//!
//! The context outlives a single [`Interpreter::exec`] on purpose. `exec` is
//! how a model thinks out loud — it snapshots, keeps a node in a variable,
//! looks at it, then acts — and a context that reset between calls would make
//! every step re-derive its own world. `globalThis` is the scratchpad;
//! [`Interpreter::reset`] is the only thing that clears it.
//!
//! Only three bindings cross into JavaScript: `__call`, `__log` and `__help`.
//! The API a script actually sees is `prelude.js`, written in JavaScript,
//! because a surface the model reads should be in the language the model
//! writes.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Function, Runtime};
use serde_json::Value;

use crate::error::HarnessError;
use crate::protocol::Cmd;
use crate::pump::PumpClient;

/// The declaration file `app.help()` hands back, and the only description of
/// the API there is.
pub const API_DTS: &str = include_str!("api.d.ts");

const PRELUDE: &str = include_str!("prelude.js");

/// Captured `console` output is truncated to this many bytes. A model that
/// logged a whole track list does not need all of it back, and an 8KB budget
/// leaves room for the result beside it.
const STDOUT_BUDGET: usize = 8 * 1024;

/// What one `exec` produced.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExecResult {
    /// The last expression's value as JSON — not a string of it, so a script
    /// that ends in `snapshot.nodes.length` gets a number back.
    pub result: Value,
    pub stdout: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The frame current when `exec` finished, so the caller can tell whether
    /// the UI moved under it.
    pub frame: u64,
}

pub struct Interpreter {
    runtime: Runtime,
    context: Context,
    client: PumpClient,
    stdout: Rc<RefCell<Vec<String>>>,
    /// Milliseconds since the epoch at which the running script must stop, or
    /// 0 for "no deadline". Read by the interrupt handler, which QuickJS calls
    /// between bytecode operations — the only way to stop a `while (true)`.
    deadline: Arc<AtomicU64>,
}

impl Interpreter {
    pub fn new(client: PumpClient) -> Result<Self, HarnessError> {
        let runtime = Runtime::new().map_err(js_setup_error)?;
        let context = Context::full(&runtime).map_err(js_setup_error)?;
        let deadline = Arc::new(AtomicU64::new(0));

        let watch = deadline.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || {
            let at = watch.load(Ordering::Relaxed);
            at != 0 && now_millis() > at
        })));

        let mut interpreter = Self {
            runtime,
            context,
            client,
            stdout: Rc::new(RefCell::new(Vec::new())),
            deadline,
        };
        interpreter.install()?;
        Ok(interpreter)
    }

    /// Throw the scratchpad away and start again. The pump is reset too, so a
    /// script and the app it was driving come back in step.
    pub fn reset(&mut self) -> Result<(), HarnessError> {
        self.client.call(Cmd::Reset)?;
        self.context = Context::full(&self.runtime).map_err(js_setup_error)?;
        self.stdout.borrow_mut().clear();
        self.install()
    }

    /// Bind the three Rust functions and run the prelude over them.
    fn install(&mut self) -> Result<(), HarnessError> {
        let client = self.client.clone();
        let stdout = self.stdout.clone();
        self.context
            .with(|ctx| -> rquickjs::Result<()> {
                let globals = ctx.globals();
                globals.set(
                    "__call",
                    Function::new(
                        ctx.clone(),
                        move |ctx: Ctx<'_>, cmd: String, args: String| {
                            dispatch(&client, &cmd, &args).map_err(|error| throw(&ctx, &error))
                        },
                    )?,
                )?;
                globals.set(
                    "__log",
                    Function::new(ctx.clone(), move |line: String| {
                        stdout.borrow_mut().push(line);
                    })?,
                )?;
                globals.set(
                    "__help",
                    Function::new(ctx.clone(), || API_DTS.to_string())?,
                )?;
                ctx.eval::<(), _>(PRELUDE)
            })
            .map_err(js_setup_error)
    }

    /// Run one script. Never fails: a thrown exception is part of the result,
    /// because a model needs to see its own mistake to fix it.
    pub fn exec(&mut self, code: &str, timeout: Duration) -> ExecResult {
        self.stdout.borrow_mut().clear();
        self.deadline
            .store(now_millis() + timeout.as_millis() as u64, Ordering::Relaxed);
        let started = Instant::now();

        let outcome =
            self.context.with(
                |ctx| match ctx.eval::<rquickjs::Value, _>(code).catch(&ctx) {
                    Ok(value) => Ok(to_json(&ctx, value)),
                    Err(error) => Err(describe(error)),
                },
            );

        self.deadline.store(0, Ordering::Relaxed);

        let (result, error) = match outcome {
            Ok(value) => (value, None),
            // An interrupted script surfaces as an uncatchable exception with
            // no message of its own, so say what actually happened.
            Err(message) if started.elapsed() >= timeout => (
                Value::Null,
                Some(format!("Timeout: exec exceeded {timeout:?} ({message})")),
            ),
            Err(message) => (Value::Null, Some(message)),
        };

        ExecResult {
            result,
            stdout: self.take_stdout(),
            error,
            frame: self.frame(),
        }
    }

    /// The current frame, or 0 if the pump cannot answer — this stamps a
    /// result that has already been produced, so a wedged app must not turn
    /// into a second failure on top of the first.
    fn frame(&self) -> u64 {
        self.client
            .call(Cmd::CurrentFrame)
            .ok()
            .and_then(|value| value.get("frame").and_then(Value::as_u64))
            .unwrap_or_default()
    }

    /// Join the captured lines, dropping the middle if the script was chatty.
    /// The head and the tail are what carry meaning — the head says what the
    /// script set out to do, the tail says where it got to.
    fn take_stdout(&self) -> String {
        let lines = std::mem::take(&mut *self.stdout.borrow_mut());
        let joined = lines.join("\n");
        if joined.len() <= STDOUT_BUDGET {
            return joined;
        }

        let half = STDOUT_BUDGET / 2;
        let mut head = 0;
        let mut kept_head = 0;
        for line in &lines {
            if head + line.len() + 1 > half {
                break;
            }
            head += line.len() + 1;
            kept_head += 1;
        }
        let mut tail = 0;
        let mut kept_tail = 0;
        for line in lines[kept_head..].iter().rev() {
            if tail + line.len() + 1 > half {
                break;
            }
            tail += line.len() + 1;
            kept_tail += 1;
        }
        let elided = lines.len() - kept_head - kept_tail;
        let mut out = lines[..kept_head].join("\n");
        out.push_str(&format!("\n[{elided} lines elided]\n"));
        out.push_str(&lines[lines.len() - kept_tail..].join("\n"));
        out
    }
}

/// Turn a `__call` from the prelude into a [`Cmd`] and run it.
///
/// The command name and its arguments arrive as two strings so that the JS
/// side owns the whole API shape: adding an option to `click` is a change to
/// `prelude.js` and `protocol.rs`, and nothing in between.
fn dispatch(client: &PumpClient, cmd: &str, args: &str) -> Result<String, HarnessError> {
    let mut value: Value = serde_json::from_str(args)
        .map_err(|error| HarnessError::BadCall(format!("{cmd}: bad arguments: {error}")))?;
    value
        .as_object_mut()
        .ok_or_else(|| HarnessError::BadCall(format!("{cmd}: arguments must be an object")))?
        .insert("cmd".into(), Value::String(cmd.to_string()));
    let cmd: Cmd = serde_json::from_value(value)
        .map_err(|error| HarnessError::BadCall(format!("{cmd}: {error}")))?;
    let result = client.call(cmd)?;
    Ok(result.to_string())
}

/// Raise a harness failure as a real `Error` object rather than a bare string,
/// so that a script can `catch` it, read `.message`, and see a stack.
fn throw(ctx: &Ctx<'_>, error: &HarnessError) -> rquickjs::Error {
    match rquickjs::Exception::from_message(ctx.clone(), &error.to_string()) {
        Ok(exception) => ctx.throw(exception.into_value()),
        Err(error) => error,
    }
}

/// What the model reads when its script fails, so it has to be the message and
/// the stack — not rquickjs's `Debug` of its own wrapper types.
fn describe(error: CaughtError<'_>) -> String {
    match error {
        CaughtError::Exception(exception) => {
            let message = exception
                .message()
                .unwrap_or_else(|| "uncaught exception".to_string());
            match exception.stack() {
                Some(stack) if !stack.trim().is_empty() => format!("{message}\n{}", stack.trim()),
                _ => message,
            }
        }
        // A script can `throw` any value at all; JSON is the honest rendering
        // of one that is not an `Error`.
        CaughtError::Value(value) => value
            .ctx()
            .json_stringify(value.clone())
            .ok()
            .flatten()
            .and_then(|text| text.to_string().ok())
            .unwrap_or_else(|| format!("{value:?}")),
        CaughtError::Error(error) => error.to_string(),
    }
}

/// JSON of a completion value. `undefined` has no JSON form, and neither does
/// a function or a cycle — all of those become `null` rather than an error,
/// because the script itself succeeded.
fn to_json<'js>(ctx: &Ctx<'js>, value: rquickjs::Value<'js>) -> Value {
    ctx.json_stringify(value)
        .ok()
        .flatten()
        .and_then(|text| text.to_string().ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

fn js_setup_error(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::BadCall(format!("could not start the interpreter: {error}"))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
