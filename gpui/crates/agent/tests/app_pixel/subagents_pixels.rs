//! Reference captures of the delegation surfaces.
//!
//! ```sh
//! cargo test -p gpui-agent --features app,pixel --test app_pixel \
//!     subagents -- --ignored --nocapture
//! ```
//!
//! Four shots of one delegation: the chip while the child is working (with the
//! floating pill counting it), the dialog's list, the child's own transcript
//! morphed into the same card, and the chip once the report has landed.
//!
//! `#[ignore]`d because it is a *generator*, not an assertion: it creates a GPU
//! device and overwrites files in the repository. The turn is the same script
//! `chat/subagents.rs` asserts on, so the picture and the assertions cannot
//! describe different delegations.

#![cfg(all(feature = "app", feature = "pixel"))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_agent::Mode;
use serde_json::Value;

#[path = "../support/chat.rs"]
mod chat;

/// Room for the dialog *and* the panel behind it: the point of the pill and the
/// chip is that they are read together.
const WINDOW: (f32, f32) = (1440., 960.);

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the repository root is above this crate")
}

fn shot(result: &Value, name: &str) {
    let path = result["path"].as_str().expect("a shot has a path");
    let out = root().join(format!("harness/gauntlet-chat/gpui-subagent-{name}.png"));
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
fn the_delegation_surfaces_are_captured() {
    let mut session = chat::delegating_session(Mode::Pixel, WINDOW);
    let result = session.app.exec(
        &format!(
            r#"
            {until}
            {open}
            {send}

            // The chip with no output yet, and the pill counting it. One frame
            // carries both, which is the pair worth looking at.
            until("the subagent pill", (s) =>
                s.find({{ role: "button", label: "1 subagent working" }}) !== undefined);
            app.frames(4, {{ waitMs: 30 }});
            const working = app.screenshot();

            app.click(app.snapshot().find({{ role: "button", label: "1 subagent working" }}));
            until("the subagents dialog", (s) =>
                s.findAll({{ role: "card" }}).some((n) => n.label.indexOf({description:?}) >= 0));
            // Past the morph's own entrance, so the card is at rest.
            app.frames(10, {{ waitMs: 30 }});
            const listed = app.screenshot();

            until("the turn end", (s) =>
                !s.findAll({{ role: "text" }}).some((n) => n.label === "Working"
                    || n.label === "Sending"));
            app.click(app.snapshot().findAll({{ role: "card" }})
                .find((n) => n.label.indexOf({description:?}) >= 0));
            until("the child's transcript", (s) =>
                s.findAll({{ role: "text" }}).some((n) => n.label.indexOf({answer:?}) >= 0));
            app.frames(10, {{ waitMs: 30 }});
            const child = app.screenshot();

            // By the controls, not by Escape — see the note in
            // `chat/subagents.rs`: no dialog's keyboard is reachable here.
            app.click(app.snapshot().find({{ role: "button", label: "Back" }}));
            app.frames(8, {{ waitMs: 30 }});
            app.click(app.snapshot().find({{ role: "button", label: "Close" }}));
            app.frames(10, {{ waitMs: 30 }});
            const finished = app.screenshot();

            ({{ working, listed, child, finished }})
        "#,
            until = chat::UNTIL,
            open = chat::open_chat("chat-subagent"),
            send = chat::send(),
            description = chat::SUBAGENT_DESCRIPTION,
            answer = chat::SUBAGENT_ANSWER,
        ),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    for name in ["working", "listed", "child", "finished"] {
        shot(&result.result[name], name);
    }
}
