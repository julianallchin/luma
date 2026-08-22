//! The settings screen, driven end to end.
//!
//! Unlike `harness.rs` — which tests the harness against a view of its own —
//! this drives the *real* `luma-app` against a disposable library, because the
//! thing under test is the round trip: a click writes through the dispatch
//! seam, and the next read of that seam is what the screen redraws from. A
//! test that stubbed the library would prove only that a label changed.

#![cfg(feature = "app")]

// Only for `support::script` — this test seeds its own fixture, but the
// navigation helpers it drives the app with are the suite's, not its own.
mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;

fn harness() -> Harness {
    support::Fixture::new("settings", 1, vec![])
        .without_track()
        .open(Mode::Headless)
}

/// Open settings, switch to AI, pick the other model, and read the picker
/// back — then leave, come back, and read it back a second time. The second
/// read is the one that matters: it is served by a fresh `get_settings`, so it
/// can only say what the database says.
const SCRIPT: &str = r#"
    nav.venue("Test Venue");

    function openSettings() {
        app.click(app.snapshot().find({ role: "button", label: "Settings" }));
        app.frames(4);
        app.click(app.snapshot().find({ role: "toggle", label: "AI" }));
        return app.snapshot();
    }

    const opened = openSettings();
    const before = opened.find({ role: "select", label: "Kimi K3 Fast" });

    // Open the picker, then choose the option that is not selected. The pick
    // is a write plus a re-read on a runtime gpui does not own — waited for
    // by its result, not by a frame count.
    app.click(before);
    app.click(app.snapshot().find({ role: "button", label: "Claude Opus 5" }));
    until("the picked model", (s) =>
        s.find({ role: "select", label: "Claude Opus 5" }) !== undefined);

    const chosen = app.snapshot().findAll({ role: "select" }).map((n) => n.label);

    // Back to the screen settings were opened over.
    nav.dismiss();
    const home = app.snapshot().nodes.map((n) => n.label);

    // …and in again, which re-reads the seam.
    const reopened = openSettings().findAll({ role: "select" }).map((n) => n.label);

    ({ before: before.label, chosen, home, reopened })
"#;

#[test]
fn the_model_picker_writes_through_the_seam_and_reads_back() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(60));
    assert_eq!(result.error, None, "script failed");
    let out: Value = result.result;

    // The default the settings schema declares.
    assert_eq!(out["before"], "Kimi K3 Fast");
    // Provider is untouched — the gateway default — and the model is the pick.
    assert_eq!(
        out["chosen"],
        serde_json::json!(["Vercel AI Gateway", "Claude Opus 5"])
    );
    // Back landed on the venue shell settings covered, with its venue intact.
    assert!(
        out["home"]
            .as_array()
            .unwrap()
            .iter()
            .any(|label| label == "Test Venue")
            && !out["home"]
                .as_array()
                .unwrap()
                .iter()
                .any(|label| label == "SETTINGS"),
        "Back did not return to the venue shell: {:?}",
        out["home"]
    );
    // The choice survived a fresh `get_settings`.
    assert_eq!(
        out["reopened"],
        serde_json::json!(["Vercel AI Gateway", "Claude Opus 5"])
    );
}
