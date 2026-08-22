//! Outside-in contract for the workspace's new-tab menu and unified closes.

#![cfg(feature = "app")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

fn harness() -> Harness {
    Fixture::new(
        "tab-chrome",
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0)],
    )
    .with_rig()
    .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    function menu() {
        app.click(app.snapshot().find({ role: "button", label: "new-tab" }));
        app.frames(2);
        const shot = app.snapshot();
        return {
            shot,
            choices: ["Universe setup", "Pattern editor", "Track editor", "Visualizer"]
                .map((label) => shot.find({ role: "button", label })),
            reasons: shot.findAll({ role: "text" }).map((node) => node.label),
        };
    }

    nav.trackEditor("Test Venue", "Aurora");
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);

    const first = menu();
    // Opening the selected track is a reveal, not a duplicate.
    app.click(first.choices[2]);
    app.frames(2);
    const trackChipsAfterReveal = app.snapshot().findAll({ role: "button", label: "Aurora" }).length;

    nav.pattern("Strobe");
    until("the pattern tab", (s) => s.find({ role: "button", label: "Strobe" }) !== undefined);
    const second = menu();
    app.click(second.choices[1]);
    app.frames(2);
    const patternChipsAfterReveal = app.snapshot().findAll({ role: "button", label: "Strobe" }).length;

    const third = menu();
    app.click(third.choices[0]);
    until("the universe tab", (s) =>
        s.find({ role: "card", label: "Test Venue Universe setup" }) !== undefined);
    app.action("luma::CloseTab");
    app.frames(2);

    // Middle click routes through the same close path as the keyboard.
    app.click(app.snapshot().find({ role: "button", label: "Strobe" }), { button: "middle" });
    app.frames(2);
    const finalButtons = app.snapshot().findAll({ role: "button" }).map((node) => node.label);

    ({
        firstEnabled: first.choices.map((node) => node.enabled),
        firstReasons: first.reasons,
        secondEnabled: second.choices.map((node) => node.enabled),
        trackChipsAfterReveal,
        patternChipsAfterReveal,
        finalButtons,
    })
"#;

#[test]
fn menu_prerequisites_idempotent_opens_and_close_gestures_share_one_path() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    assert_eq!(
        out["firstEnabled"],
        serde_json::json!([true, false, true, true])
    );
    assert!(
        out["firstReasons"].as_array().is_some_and(|reasons| reasons
            .iter()
            .any(|reason| reason == "Select a pattern first")),
        "the disabled pattern choice did not explain itself: {out:#}"
    );
    assert_eq!(
        out["secondEnabled"],
        serde_json::json!([true, true, true, true])
    );
    assert_eq!(out["trackChipsAfterReveal"], 1);
    assert_eq!(out["patternChipsAfterReveal"], 1);
    let final_buttons = out["finalButtons"].as_array().unwrap();
    assert!(final_buttons.iter().any(|label| label == "Aurora"));
    assert!(!final_buttons.iter().any(|label| label == "Strobe"));
}
