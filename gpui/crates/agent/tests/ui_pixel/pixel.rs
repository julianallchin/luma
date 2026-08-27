//! Pixel mode, which only exists with the `pixel` feature:
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test pixel
//! ```
//!
//! It creates a GPU device, so it is not part of the default test run — but it
//! is the only mode that can answer "what did that actually look like", and an
//! untested screenshot path is a screenshot path that has rotted.
#![cfg(feature = "pixel")]

use std::sync::Arc;
use std::time::Duration;

use gpui::{div, prelude::*, px, AnyView, App, Context, Render, Window};
use gpui_agent::{Config, Harness, Mode, GPU_LIVENESS_TIMEOUT};
use luma_ui::node::{Instrument, Role};
use serde_json::Value;

struct Swatch;

impl Render for Swatch {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(luma_ui::ladder::background())
            .child(
                div()
                    .w(px(200.))
                    .h(px(100.))
                    .bg(luma_ui::ladder::primary())
                    .agent_node(Role::Card, "Swatch"),
            )
            .agent_node(Role::Text, "Screen")
    }
}

fn harness() -> Harness {
    let root: gpui_agent::RootFactory =
        Arc::new(|_: &mut Window, cx: &mut App| -> AnyView { cx.new(|_| Swatch).into() });
    Harness::headless(
        Config {
            mode: Mode::Pixel,
            call_timeout: GPU_LIVENESS_TIMEOUT,
            ..Config::default()
        },
        root,
    )
    .expect("failed to start the harness")
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, GPU_LIVENESS_TIMEOUT);
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

#[test]
fn the_window_and_one_node_can_both_be_captured() {
    let mut harness = harness();

    let whole = run(&mut harness, "app.screenshot()");
    let path = whole["path"].as_str().unwrap();
    assert!(std::path::Path::new(path).exists(), "no file at {path}");
    assert!(whole["width"].as_u64().unwrap() >= 1200);

    // Cropping to a node's bounds gives back exactly that node's box, scaled
    // to physical pixels — which is what makes "show me this button" cheap.
    let swatch = run(
        &mut harness,
        r#"app.screenshot({ node: app.snapshot().find({ label: "Swatch" }) })"#,
    );
    let scale = whole["width"].as_u64().unwrap() as f64 / 1200.;
    assert_eq!(swatch["width"].as_u64().unwrap() as f64, 200. * scale);
    assert_eq!(swatch["height"].as_u64().unwrap() as f64, 100. * scale);
}

/// Timings carry the mode that produced them, because the two are not
/// comparable: pixel feeds layout real glyph metrics, so its `drawMs` measures
/// a different amount of text work for the same tree. Neither times the GPU.
#[test]
fn timings_report_the_mode_that_produced_them() {
    let mut harness = harness();
    let report = run(
        &mut harness,
        r#"
            app.frames(3, { waitMs: 0 });
            const t = app.timings();
            ({ mode: t.mode, timed: t.frames.length > 0 })
        "#,
    );
    assert_eq!(report["mode"], "pixel");
    assert_eq!(report["timed"], true);
}

/// Pixel mode exists to be the *same* harness with better parts, so the node
/// tree has to be identical to what headless mode reports.
#[test]
fn the_node_tree_is_the_same_as_headless() {
    let pixel = run(
        &mut harness(),
        r#"app.snapshot().nodes.map((n) => n.label)"#,
    );

    let root: gpui_agent::RootFactory =
        Arc::new(|_: &mut Window, cx: &mut App| -> AnyView { cx.new(|_| Swatch).into() });
    let mut plain = Harness::headless(Config::default(), root).unwrap();
    let headless = run(&mut plain, r#"app.snapshot().nodes.map((n) => n.label)"#);

    assert_eq!(pixel, headless);
}
