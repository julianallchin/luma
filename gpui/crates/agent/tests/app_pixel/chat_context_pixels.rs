#![cfg(all(feature = "app", feature = "pixel"))]

//! Captures of the context gauge: the strip at rest, and the card open.
//!
//! A capture generator rather than a gate — what it is checking is that a ring
//! painted with `paint_path` at 14px reads as a ring and not as a smudge, and
//! that the card's monospace column lines up. Neither is a thing a node tree
//! can see, which is why `context_gauge` in the `chat` suite covers the
//! *values* and this covers the picture.
//!
//! `cargo test -p gpui-agent --features app,pixel --test app_pixel
//!  chat_context -- --ignored --nocapture`

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_agent::Mode;
use serde_json::Value;

#[path = "../support/chat.rs"]
mod chat;

const WINDOW: (f32, f32) = (1280., 900.);

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the repository root is above this crate")
}

fn shot(result: &Value, name: &str) {
    let path = result["path"].as_str().expect("a shot has a path");
    let out = root().join(format!("harness/gauntlet-chat/gpui-context-{name}.png"));
    std::fs::create_dir_all(out.parent().expect("a parent")).expect("failed to make the directory");
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
fn the_context_gauge_is_captured_at_rest_and_open() {
    let mut session = chat::session(Mode::Pixel, WINDOW);
    let result = session.app.exec(
        &format!(
            r#"
            {until}
            {open}
            {send}
            until("the turn end", (s) =>
                !s.findAll({{ role: "text" }}).some((n) => n.label === "Working"
                    || n.label === "Sending"));
            app.frames(4, {{ waitMs: 40 }});

            // The whole panel, so the ring is seen where it lives — a crop to
            // 14px would show a ring and prove nothing about its weight beside
            // the text it sits with.
            const settled = app.screenshot();

            const gauge = app.snapshot().find({{ role: "text", label: {reading:?} }});
            app.scroll(gauge, {{ dx: 0, dy: 0 }});
            app.frames(4, {{ waitMs: 40 }});
            const open = app.screenshot();
            ({{ settled, open }})
        "#,
            until = chat::UNTIL,
            open = chat::open_chat("chat-context"),
            send = chat::send(),
            reading = chat::CONTEXT_READING,
        ),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "capture failed:\n{}", result.stdout);
    for name in ["settled", "open"] {
        shot(&result.result[name], name);
    }
}
