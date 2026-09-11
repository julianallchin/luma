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

            const gauge = app.snapshot().find({{ role: "button", label: {reading:?} }});
            app.click(gauge);
            app.frames(4, {{ waitMs: 40 }});
            const open = app.screenshot();
            ({{ settled, open }})
        "#,
            until = chat::UNTIL,
            open = chat::open_chat(),
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

#[test]
#[ignore = "capture generator: needs a GPU"]
fn model_picker_with_effort_slider() {
    let mut session = chat::session(Mode::Pixel, WINDOW);
    let result = session.app.exec(
        &format!(r#"
            {until}
            {open}
            until("model picker ready", (s) => s.find({{ role: "select", label: "Vercel AI Gateway · Claude Opus 5" }}) !== undefined);
            app.frames(8, {{ waitMs: 40 }});
            app.click(app.snapshot().find({{ role: "select", label: "Vercel AI Gateway · Claude Opus 5" }}));
            until("effort slider", (s) => s.find({{ role: "slider", label: "Reasoning effort" }}) !== undefined);
            app.frames(4, {{ waitMs: 40 }});
            app.screenshot()
        "#, until = chat::UNTIL, open = chat::open_chat()),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "capture failed: {}", result.stdout);
    shot(&result.result, "model-picker");
}

/// The models view: provider chips over the search, over priced rows — then
/// the same view narrowed by a query. Reads the live gateway list, so it needs
/// a network as well as a GPU.
#[test]
#[ignore = "capture generator: needs a GPU and a network"]
fn model_list_with_search() {
    let mut session = chat::session(Mode::Pixel, WINDOW);
    let result = session.app.exec(
        &format!(r#"
            {until}
            {open}
            const trigger = {{ role: "select", label: "Vercel AI Gateway · Claude Opus 5" }};
            until("model picker ready", (s) => s.find(trigger) !== undefined);
            app.frames(8, {{ waitMs: 40 }});
            app.click(app.snapshot().find(trigger));
            app.frames(8, {{ waitMs: 40 }});
            if (!app.snapshot().find({{ role: "button", label: "Choose model" }})) {{
                app.click(app.snapshot().find(trigger));
            }}
            until("effort card", (s) => s.find({{ role: "button", label: "Choose model" }}) !== undefined);
            app.click(app.snapshot().find({{ role: "button", label: "Choose model" }}));
            const priced = (s) => s.findAll({{ role: "button" }}).filter((n) => n.label.includes(" in · ")).length;
            until("priced rows", (s) => priced(s) > 5);
            app.frames(12, {{ waitMs: 40 }});
            const models = app.screenshot();
            app.type(app.snapshot().find({{ role: "input", label: "Search models…" }}), "kimi");
            until("narrowed", (s) => priced(s) > 0 && priced(s) < 12);
            app.frames(12, {{ waitMs: 40 }});
            const rows = app.snapshot().findAll({{ role: "button" }}).map((n) => n.label).filter((l) => l.includes(" in · "));
            ({{ models, search: app.screenshot(), rows }})
        "#, until = chat::UNTIL, open = chat::open_chat()),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "capture failed: {}", result.stdout);
    println!("{}", result.result["rows"]);
    for name in ["models", "search"] {
        shot(&result.result[name], &format!("model-list-{name}"));
    }
}
