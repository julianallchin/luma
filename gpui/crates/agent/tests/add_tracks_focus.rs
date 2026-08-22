//! Production AddTracks focus handoff during a live morph.

#![cfg(feature = "app")]

mod support;

use std::collections::HashMap;
use std::time::Duration;

use gpui_agent::Mode;
use serde_json::{json, Value};

#[test]
fn morph_focus_moves_to_modal_scope_then_commits_to_the_target_route() {
    std::env::set_var("LUMA_MOTION", "on");
    let mut harness = support::Fixture::new("add-tracks-focus", 1, vec![])
        .with_source_fixture(luma_app::SourceAdapterFixture {
            library: json!({"trackCount": 0}),
            playlists: json!([]),
            tracks: json!([]),
            playlist_tracks: HashMap::new(),
            searches: HashMap::new(),
        })
        .with_source_fixture_delay(Duration::from_millis(400))
        .open(Mode::Headless);
    let script = support::script(
        r#"
        const focused = (s) => s.findAll((node) => node.focused);
        const modalFocused = (s) => s.findAll({ role: "card", label: "Add tracks dialog" })
            .some((card) => card.focused);

        nav.venue("Test Venue");
        // This test turns motion on for the dialog. Let the unrelated sidebar
        // entrance finish so its clipped Add button is a valid entry target.
        app.frames(20);
        nav.step("the add-track affordance", "button", "Add track");
        const browser = until("focused all-Luma search", (s) =>
            s.find({ role: "input", label: "Search all tracks…" })?.focused === true
                && s.find({ role: "button", label: "Import tracks" }) !== undefined);
        app.click(browser.find({ role: "button", label: "Import tracks" }));
        app.frames(1);
        const pickerFlight = app.snapshot();
        const pickerFlightFocus = focused(pickerFlight);
        const picker = until("committed picker focus", (s) =>
            s.find({ role: "button", label: "Engine DJ" })?.focused === true);

        app.click(picker.find({ role: "button", label: "Rekordbox" }));
        app.frames(1);
        const sourceFlight = app.snapshot();
        const sourceFlightFocus = focused(sourceFlight);
        const source = until("committed source focus", (s) =>
            s.find({ role: "input", label: "Search source…" })?.focused === true);

        ({
            pickerModal: modalFocused(pickerFlight),
            pickerOnlyModal: pickerFlightFocus.every((node) => node.role === "card"),
            pickerTarget: picker.find({ role: "button", label: "Engine DJ" })?.focused === true,
            sourceModal: modalFocused(sourceFlight),
            sourceOnlyModal: sourceFlightFocus.every((node) => node.role === "card"),
            sourceTarget: source.find({ role: "input", label: "Search source…" })?.focused === true,
        })
    "#,
    );
    let result = harness.exec(&script, Duration::from_secs(180));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    for key in [
        "pickerModal",
        "pickerOnlyModal",
        "pickerTarget",
        "sourceModal",
        "sourceOnlyModal",
        "sourceTarget",
    ] {
        assert_eq!(out[key], true, "focus contract failed at {key}: {out:#}");
    }
}
