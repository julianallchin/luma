//! Production AddTracks focus handoff during a live morph.

#![cfg(feature = "app")]

use super::support;

use std::collections::HashMap;
use std::time::Duration;

use gpui_agent::Mode;
use serde_json::{json, Value};

#[test]
fn morph_focus_moves_to_modal_scope_then_commits_to_the_target_route() {
    let mut harness = support::Fixture::new("add-tracks-focus", 1, vec![])
        .with_motion()
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
        // This test turns motion on, so the sidebar's entrance has to finish
        // before its clipped Add button is a valid entry target. (The venue
        // dialog's own exit is already waited out by `nav.venue`.)
        app.frames(20, { waitMs: 10 });
        nav.step("the add-track affordance", "button", "Add track");
        const browser = until("focused all-Luma search", (s) =>
            s.find({ role: "input", label: "Search all tracks…" })?.focused === true
                && s.find({ role: "button", label: "Import tracks" }) !== undefined);
        // Choosing a source is a menu now, not a route — it hangs off the
        // header chip and moves no focus, so there is exactly one morph left to
        // watch: the browser giving way to the source library.
        app.click(browser.find({ role: "button", label: "Import tracks" }));
        const menu = until("the import-source menu", (s) =>
            s.find({ role: "row", label: "Rekordbox" }) !== undefined);

        app.click(menu.find({ role: "row", label: "Rekordbox" }));
        app.frames(1);
        const sourceFlight = app.snapshot();
        const sourceFlightFocus = focused(sourceFlight);
        const source = until("committed source focus", (s) =>
            s.find({ role: "input", label: "Search source…" })?.focused === true);

        ({
            // A menu is not a route — it opens in place, morphing nothing, so
            // the browser behind it is still the mounted route.
            menuKeptRoute:
                menu.find({ role: "input", label: "Search all tracks…" }) !== undefined,
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
        "menuKeptRoute",
        "sourceModal",
        "sourceOnlyModal",
        "sourceTarget",
    ] {
        assert_eq!(out[key], true, "focus contract failed at {key}: {out:#}");
    }
}
