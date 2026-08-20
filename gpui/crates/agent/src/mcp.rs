//! MCP over stdio, by hand.
//!
//! MCP's stdio transport is newline-delimited JSON-RPC 2.0, and a server that
//! exposes two tools needs four methods of it: `initialize`, `tools/list`,
//! `tools/call`, and `ping`. That is small enough that writing it is cheaper
//! than owning an SDK — `rmcp` is async-first and would pull a Tokio runtime
//! next to a harness whose entire point is one blocking thread per stage.
//!
//! Exactly two tools are exposed. Everything a driver might want is reachable
//! from code, and a tool per verb would be a second API to keep in step with
//! `api.d.ts`.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{json, Value};

use crate::Harness;

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
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write(&mut output, parse_error(&error.to_string()))?;
                continue;
            }
        };
        // A notification has no id and takes no response — `initialized` is
        // the only one that reaches us, and it wants silence, not an ack.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        write(&mut output, respond(harness, id, method, params))?;
    }
    Ok(())
}

fn write(output: &mut impl Write, message: Value) -> std::io::Result<()> {
    writeln!(output, "{message}")?;
    output.flush()
}

fn respond(harness: &mut Harness, id: Value, method: &str, params: Value) -> Value {
    match method {
        "initialize" => ok(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "gpui-agent", "version": env!("CARGO_PKG_VERSION") },
            }),
        ),
        "ping" => ok(id, json!({})),
        "tools/list" => ok(id, json!({ "tools": tools() })),
        "tools/call" => call(harness, id, params),
        other => error(id, -32601, &format!("unknown method: {other}")),
    }
}

fn call(harness: &mut Harness, id: Value, params: Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "exec" => {
            let Some(code) = arguments.get("code").and_then(Value::as_str) else {
                return ok(
                    id,
                    tool_error("exec: `code` is required and must be a string"),
                );
            };
            let timeout = arguments
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map(Duration::from_millis)
                .unwrap_or(DEFAULT_EXEC_TIMEOUT);
            let result = harness.exec(code, timeout);
            // A script that threw is a *result*, not a transport failure: the
            // model needs the stdout and the frame alongside the message to
            // work out what went wrong.
            let is_error = result.error.is_some();
            ok(id, content(json!(result), is_error))
        }
        "reset" => match harness.reset() {
            Ok(()) => ok(id, content(json!({ "ok": true }), false)),
            Err(error) => ok(id, tool_error(&error.to_string())),
        },
        other => ok(id, tool_error(&format!("unknown tool: {other}"))),
    }
}

fn tools() -> Value {
    json!([
        {
            "name": "exec",
            "description": "Run JavaScript against the running app. `globalThis` persists \
                            between calls. Start with `app.help()` for the full API. Returns \
                            {result, stdout, error?, frame}, where `result` is the value of the \
                            last expression.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "The script to run." },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Deadline for this script. Defaults to 30000.",
                    },
                },
                "required": ["code"],
            },
        },
        {
            "name": "reset",
            "description": "Tear the app down and build it again, and throw away everything \
                            the interpreter was holding.",
            "inputSchema": { "type": "object", "properties": {} },
        },
    ])
}

fn content(value: Value, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": value.to_string() }],
        "isError": is_error,
    })
}

fn tool_error(message: &str) -> Value {
    content(json!({ "error": message }), true)
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn parse_error(message: &str) -> Value {
    error(Value::Null, -32700, message)
}
