//! Focus regression for the production gpui-component Root + modal trap.

#![cfg(feature = "app")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(60));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

#[test]
fn modal_traps_both_tab_directions_and_restores_the_exact_opener() {
    let mut harness = Fixture::new(
        "dialog-focus",
        8,
        vec![Clip::new("pattern-strobe", "Strobe", 1.0, 4.0)],
    )
    .open(Mode::Headless);

    let script = support::script(
        r#"
        nav.venue("Test Venue");
        const poll = (description, predicate) => {
            for (let i = 0; i < 120; i++) {
                const shot = app.snapshot();
                if (predicate(shot)) return shot;
                app.frames(1, { waitMs: 25 });
            }
            throw new Error(`never saw ${description}`);
        };

        poll("the venue track search", (s) =>
            s.find({ role: "input", label: "Search tracks…" }) !== undefined);
        app.click(app.snapshot().find({ role: "input", label: "Search tracks…" }));
        const openerBefore = app.snapshot().find({ role: "input", label: "Search tracks…" });

        app.action("luma::OpenPatterns");
        poll("the focusable pattern row", (s) =>
            s.find({ role: "row", label: "Strobe" }) !== undefined);
        const initialDialog = app.snapshot().find({ role: "card", label: "Pattern dialog" });

        app.key("tab");
        app.frames(2);
        const first = app.snapshot().find({ role: "button", label: "Close" });
        const afterForward = app.snapshot().find({ role: "card", label: "Pattern dialog" });
        const focusedAfterForward = app.snapshot().nodes
            .filter((node) => node.focused)
            .map((node) => `${node.role}:${node.label}`);

        app.key("shift-tab");
        app.frames(2);
        const last = app.snapshot().find({ role: "row", label: "Strobe" });
        const afterReverse = app.snapshot().find({ role: "card", label: "Pattern dialog" });

        app.key("tab");
        app.frames(2);
        const wrapped = app.snapshot().find({ role: "button", label: "Close" });

        app.key("escape");
        app.frames(2);
        const restored = app.snapshot().find({ role: "input", label: "Search tracks…" });
        const dismissed = app.snapshot().find({ role: "card", label: "Pattern dialog" });

        ({ openerBefore, initialDialog, first, afterForward, focusedAfterForward, last, afterReverse, wrapped, restored, dismissed })
        "#,
    );
    let out = run(&mut harness, &script);

    assert_eq!(out["openerBefore"]["focused"], true);
    assert_eq!(out["initialDialog"]["focused"], true);
    assert_eq!(
        out["first"]["focused"], true,
        "forward Tab focused {:?}",
        out["focusedAfterForward"]
    );
    assert_eq!(out["afterForward"]["focused"], true);
    assert_eq!(out["last"]["focused"], true);
    assert_eq!(out["afterReverse"]["focused"], true);
    assert_eq!(out["wrapped"]["focused"], true);
    assert_eq!(out["restored"]["focused"], true);
    assert!(out["dismissed"].is_null());
}
