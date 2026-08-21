//! The chat gauntlet: reference captures of the agent panel.
//!
//! ```sh
//! cargo test -p gpui-agent --features app,pixel --test gauntlet_chat -- --ignored --nocapture
//! ```
//!
//! Three shots of one turn — idle, mid-reply, settled — written beside
//! `harness/gauntlet-chat/style-spec.md` and its `comet-ref-*` plates, which
//! are the bar this surface is measured against. `#[ignore]`d because it is a
//! *generator*, not an assertion: it creates a GPU device and overwrites files
//! in the repository. Nothing here fails on a pixel change; a human (or a
//! critic) compares the pair.
//!
//! The turn is the same script `agent_chat.rs` asserts on, so the picture and
//! the assertions cannot describe different replies.

#![cfg(all(feature = "app", feature = "pixel"))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_agent::Mode;
use serde_json::Value;

#[path = "support/chat.rs"]
mod chat;

/// Sized so the panel sits over a graph with room to read both. Taller than
/// the assertion suite's window: a capture wants the transcript to breathe.
const WINDOW: (f32, f32) = (1440., 960.);

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the repository root is above this crate")
}

fn shot(result: &Value, name: &str) {
    let path = result["path"].as_str().expect("a shot has a path");
    let out = root().join(format!("harness/gauntlet-chat/gpui-chat-{name}.png"));
    std::fs::copy(path, &out).expect("failed to write the capture");
    println!(
        "{}  {} x {}",
        out.display(),
        result["width"],
        result["height"]
    );
}

#[test]
#[ignore = "capture generator: needs a GPU and writes into harness/gauntlet-chat"]
fn the_chat_panel_is_captured_across_one_turn() {
    let mut session = chat::session(Mode::Pixel, WINDOW);
    let result = session.app.exec(
        &format!(
            r#"
            {open}
            const idle = app.screenshot();
            {send}
            // Inside the scripted tool's latency: prose painted, chip running.
            until("the tool call start", (s) => chips(s).some((c) => c.startsWith("Running")));
            // A beat into the veil rather than at its start: caught at the
            // first frame the whole reply is at the fade's floor and the plate
            // shows a grey block instead of a reply mid-arrival.
            app.frames(6, {{ waitMs: 30 }});
            const streaming = app.screenshot();
            until("the turn end", (s) => chips(s).some((c) => c.startsWith("Ran"))
                && !s.findAll({{ role: "text" }}).some((n) => n.label === "Working"));
            app.frames(4, {{ waitMs: 40 }});
            const finished = app.screenshot();
            ({{ idle, streaming, finished }})
        "#,
            open = chat::open_chat(chat::CAPTURED),
            send = chat::send(),
        ),
        Duration::from_secs(240),
    );
    assert_eq!(result.error, None, "capture failed:\n{}", result.stdout);

    for name in ["idle", "streaming", "finished"] {
        shot(&result.result[name], name);
    }
}
