//! MCP over stdio.
//!
//! The framing, `initialize`, `ping` and `tools/list` live in `mcp-stdio`,
//! shared with `src-tauri`'s `luma-mcp`. What is here is this harness's own
//! half: two tools, and a strictly serial loop — one blocking thread per stage
//! is the whole architecture of this crate, and a driver that could interleave
//! two scripts against one interpreter would be lying about what it drives.
//!
//! Exactly two tools are exposed. Everything a driver might want is reachable
//! from code, and a tool per verb would be a second API to keep in step with
//! `api.d.ts`.

use std::io::{BufRead, Write};
use std::time::Duration;

use mcp_stdio::{ok, result, route, text, tool, tool_error, Routed, ServerInfo, Surface};
use serde_json::{json, Value};

use crate::Harness;

const SERVER: ServerInfo = ServerInfo {
    name: "gpui-agent",
    version: env!("CARGO_PKG_VERSION"),
};

/// Default per-`exec` deadline. Long enough for a script that walks a few
/// screens, short enough that a wedged app is noticed rather than waited on.
const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);

/// Read requests from `input` and write responses to `output` until the client
/// hangs up.
pub fn serve(
    harness: &mut Harness,
    input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    let tools = tools();
    // A harness driver, not a content server: no prompts.
    let surface = Surface::new(&tools);
    for line in input.lines() {
        let response = match route(&line?, SERVER, &surface) {
            Routed::Silent => continue,
            Routed::Respond { response, .. } => response,
            Routed::Call {
                id,
                name,
                arguments,
            } => ok(&id, &call(harness, &name, &arguments)),
        };
        writeln!(output, "{response}")?;
        output.flush()?;
    }
    Ok(())
}

fn call(harness: &mut Harness, name: &str, arguments: &Value) -> Value {
    match name {
        "exec" => {
            let Some(code) = arguments.get("code").and_then(Value::as_str) else {
                return tool_error("exec: `code` is required and must be a string");
            };
            let timeout = arguments
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map_or(DEFAULT_EXEC_TIMEOUT, Duration::from_millis);
            let outcome = harness.exec(code, timeout);
            // A script that threw is a *result*, not a transport failure: the
            // model needs the stdout and the frame alongside the message to
            // work out what went wrong.
            let is_error = outcome.error.is_some();
            result(&[text(json!(outcome).to_string())], is_error)
        }
        "reset" => match harness.reset() {
            Ok(()) => result(&[text(json!({ "ok": true }).to_string())], false),
            Err(error) => tool_error(error.to_string()),
        },
        other => tool_error(format!("unknown tool: {other}")),
    }
}

fn tools() -> Value {
    json!([
        tool(
            "exec",
            "Run JavaScript against the running app. `globalThis` persists between calls. \
             Start with `app.help()` for the full API. Returns {result, stdout, error?, \
             frame}, where `result` is the value of the last expression.",
            &json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "The script to run." },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Deadline for this script. Defaults to 30000.",
                    },
                },
                "required": ["code"],
            }),
        ),
        tool(
            "reset",
            "Tear the app down and build it again, and throw away everything the \
             interpreter was holding.",
            &json!({ "type": "object", "properties": {} }),
        ),
    ])
}
