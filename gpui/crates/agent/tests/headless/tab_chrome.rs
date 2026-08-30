//! Outside-in contract for the workspace's new-tab menu and unified closes.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

fn harness() -> Harness {
    fixture("tab-chrome").open(Mode::Headless)
}

/// `name` is per-test: the fixture keys its seeded library directory by it,
/// and two harnesses on one name race for the same SQLite file.
fn fixture(name: &'static str) -> Fixture {
    Fixture::new(
        name,
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0)],
    )
    .with_rig()
}

const SCRIPT: &str = r#"
    function menu() {
        app.click(app.snapshot().find({ role: "button", label: "new-tab" }));
        app.frames(2);
        const shot = app.snapshot();
        return {
            shot,
            choices: ["Patch", "Pattern editor", "Track editor"]
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
        s.find({ role: "card", label: "Test Venue Patch" }) !== undefined);
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

    assert_eq!(out["firstEnabled"], serde_json::json!([true, false, true]));
    assert!(
        out["firstReasons"].as_array().is_some_and(|reasons| reasons
            .iter()
            .any(|reason| reason == "Select a pattern first")),
        "the disabled pattern choice did not explain itself: {out:#}"
    );
    assert_eq!(out["secondEnabled"], serde_json::json!([true, true, true]));
    assert_eq!(out["trackChipsAfterReveal"], 1);
    assert_eq!(out["patternChipsAfterReveal"], 1);
    let final_buttons = out["finalButtons"].as_array().unwrap();
    assert!(final_buttons.iter().any(|label| label == "Aurora"));
    assert!(!final_buttons.iter().any(|label| label == "Strobe"));
}

/// ⌘T brings the panel back and opens the menu on it — including from a shut
/// panel, where those are one action doing two things at once.
///
/// The `+` and its menu live only in the panel now, so ⌘T has to bring the
/// panel with them or it reaches nothing at all — the regression `empty_panel`
/// records, with tabs open instead of none.
///
/// Motion is on because the interesting frames are the ones during the panel's
/// entrance: the menu is opened while the region it belongs to is still
/// arriving, and it has to be there both then and after it settles.
#[test]
fn new_tab_opens_the_panel_and_its_menu_together() {
    let mut harness = fixture("tab-chrome-new-tab")
        .with_motion()
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
            nav.trackEditor("Test Venue", "Aurora");
            until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);

            app.action("luma::ToggleWorkspace");
            until("the panel put away", (s) =>
                s.find({ role: "card", label: "Tab strip" }) === undefined);

            app.action("luma::NewTab");
            const menu = until("the new-tab menu", (s) =>
                s.find({ role: "card", label: "New tab menu" }) !== undefined ? s : undefined);
            // And it stays: a menu that survives one frame and then vanishes as
            // the panel settles is the same bug arriving late.
            app.frames(12, { waitMs: 40 });
            const settled = app.snapshot();
            ({
                opened: menu.find({ role: "button", label: "Patch" }) !== undefined,
                stillUp: settled.find({ role: "card", label: "New tab menu" }) !== undefined,
                strip: settled.find({ role: "card", label: "Tab strip" }) !== undefined,
            })
        "#,
        ),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    assert_eq!(out["opened"], true, "⌘T opened no menu: {out:#}");
    assert_eq!(
        out["stillUp"], true,
        "the menu was dismissed while the panel it belongs to was still arriving: {out:#}"
    );
    assert_eq!(
        out["strip"], true,
        "the panel came back without its strip: {out:#}"
    );
}
