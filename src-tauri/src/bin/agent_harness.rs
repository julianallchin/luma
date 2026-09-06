//! Headless JSON-RPC harness over Luma's backend command surface.
//!
//! GPUI and local programs share `luma_lib::dispatch`. This binary makes that
//! command surface available without a window: one JSON request per line on
//! stdin, one JSON response per line on stdout. Calling inspection, editing or
//! rendering commands does not start a model turn.
//!
//! ```text
//! ->  {"id": 1, "cmd": "list_patterns", "args": {}}
//! <-  {"id": 1, "ok": [ ... ]}
//! <-  {"id": 1, "err": "message"}
//! ```
//!
//! Every command uses the same handler and argument schema as the native app.
//!
//! Deliberately absent: ArtNet (its manager needs an `AppHandle`), audio
//! devices, and the loops — nothing spawns a render loop or a sync loop here,
//! so a `RenderEngine` exists but drives nothing. Emitted events go to stderr.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

use luma_lib::dispatch;
use luma_lib::headless_host::{boot, HostConfig};

// -----------------------------------------------------------------------------
// Main loop
// -----------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("[agent_harness] fatal: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let config = HostConfig::parse_args(std::env::args().skip(1))?;
    let services = boot(&config).await?;

    eprintln!(
        "[agent_harness] ready: config={} fixtures={}",
        services.storage().path().display(),
        services.fixtures_root().display()
    );

    // Requests are dispatched concurrently, one task each, because some pairs
    // of commands only make sense that way:
    // `cancel_python_cell` exists precisely to interrupt a `run_python_cell`
    // that is still in flight, and a strictly serial loop could never deliver
    // it. Responses are matched by `id`, so completion order is free.
    // `into_shared`, not a bare `Arc::new`: the turn loop outlives the command
    // that starts it, so it needs the back-reference that attaches here. A
    // plain Arc leaves `agent_turn_start` failing on every call.
    let services = services.into_shared();
    let stdout = std::sync::Arc::new(tokio::sync::Mutex::new(std::io::stdout()));

    // stdin is blocking; read it on its own thread and feed the runtime.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    eprintln!("[agent_harness] stdin read failed: {e}");
                    return;
                }
            }
        }
    });

    while let Some(line) = rx.recv().await {
        if line.trim().is_empty() {
            continue;
        }
        let services = services.clone();
        let stdout = std::sync::Arc::clone(&stdout);
        // A malformed frame must never take the process down — the shim keeps
        // one long-lived child and would lose every in-flight call.
        tokio::spawn(async move {
            let response = match serde_json::from_str::<Value>(&line) {
                Err(e) => {
                    // A null id is unattributable by construction, so say what
                    // arrived: the only lead the shim can hand a human is the
                    // bytes that were actually read off the pipe.
                    let sample: String = line.chars().take(200).collect();
                    json!({
                        "id": Value::Null,
                        "err": format!("malformed request JSON: {e}; line was {sample:?}"),
                    })
                }
                Ok(req) => {
                    let id = req.get("id").cloned().unwrap_or(Value::Null);
                    match req.get("cmd").and_then(|c| c.as_str()) {
                        None => json!({ "id": id, "err": "request is missing `cmd`" }),
                        Some(cmd) => {
                            let args = req.get("args").cloned().unwrap_or_else(|| json!({}));
                            match dispatch::dispatch(&services, cmd, &args).await {
                                Ok(v) => json!({ "id": id, "ok": v }),
                                Err(e) => json!({ "id": id, "err": String::from(e) }),
                            }
                        }
                    }
                }
            };
            let Ok(mut buf) = serde_json::to_vec(&response) else {
                eprintln!("[agent_harness] response was not serializable");
                return;
            };
            buf.push(b'\n');
            let mut out = stdout.lock().await;
            if let Err(e) = out.write_all(&buf).and_then(|()| out.flush()) {
                eprintln!("[agent_harness] stdout write failed: {e}");
            }
        });
    }

    Ok(())
}
