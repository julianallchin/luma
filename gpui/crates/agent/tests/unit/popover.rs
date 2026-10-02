//! Shared popover motion follows the fitted anchor, including collision flips.
use std::sync::Arc;
use std::time::Duration;

use gpui::{div, prelude::*, px, size, App, Context, Window};
use gpui_agent::{Config, Harness};
use luma_ui::float;
use luma_ui::node::{Instrument, Role};

struct Popovers;

impl Render for Popovers {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut root = div().size_full();
        for (name, x, y) in [
            ("above", 160., 220.),
            ("above-flipped", 340., 10.),
            ("below", 520., 10.),
            ("below-flipped", 700., 260.),
            ("left", 820., 120.),
            ("left-flipped", 10., 120.),
        ] {
            for (phase, progress) in [("rest", 0.0), ("closing", 0.5)] {
                let label = format!("{name}-{phase}");
                let content = div()
                    .w(px(100.))
                    .h(px(60.))
                    .agent_node(Role::Card, label.clone())
                    .into_any_element();
                let popup = match name.split('-').next() {
                    Some("above") => float::anchored_above_closing(label, 20., content, progress),
                    Some("below") => float::anchored_below_closing(label, 20., content, progress),
                    _ => float::anchored_left_closing(label, 20., content, progress),
                };
                root = root.child(
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .size(px(20.))
                        .child(popup),
                );
            }
        }
        root
    }
}

#[test]
fn popover_exit_moves_toward_the_resolved_trigger_side() {
    let mut harness = Harness::headless(
        Config {
            window_size: size(px(900.), px(300.)),
            ..Config::default()
        },
        Arc::new(|_: &mut Window, cx: &mut App| cx.new(|_| Popovers).into()),
    )
    .expect("harness");
    let result = harness.exec(
        r#"
        app.frames(3);
        const bounds = (label) => {
            const node = app.snapshot().find({ role: "card", label });
            if (!node || node.bounds.width <= 0) throw new Error("missing " + label);
            return node.bounds;
        };
        const results = {};
        for (const name of ["above", "above-flipped", "below", "below-flipped", "left", "left-flipped"]) {
            const rest = bounds(name + "-rest");
            const closing = bounds(name + "-closing");
            results[name] = {
                x: rest.x, right: rest.x + rest.width, y: rest.y, bottom: rest.y + rest.height,
                dx: closing.x - rest.x, dy: closing.y - rest.y,
            };
        }
        results
    "#,
        Duration::from_secs(15),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let results = result.result;
    for (name, axis, direction) in [
        ("above", "dy", 1.),
        ("above-flipped", "dy", -1.),
        ("below", "dy", -1.),
        ("below-flipped", "dy", 1.),
        ("left", "dx", 1.),
        ("left-flipped", "dx", -1.),
    ] {
        assert_eq!(results[name][axis].as_f64(), Some(direction), "{results}");
    }
    assert!(results["above"]["bottom"].as_f64().expect("bottom") < 220.);
    assert!(results["above-flipped"]["y"].as_f64().expect("y") > 30.);
    assert!(results["below"]["y"].as_f64().expect("y") > 30.);
    assert!(results["below-flipped"]["bottom"].as_f64().expect("bottom") < 260.);
    assert!(results["left"]["right"].as_f64().expect("right") < 820.);
    assert!(results["left-flipped"]["x"].as_f64().expect("x") > 30.);
}
