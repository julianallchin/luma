//! Malformed adapter answers surface as an operable source error route.

#![cfg(feature = "app")]

use super::support;

use std::collections::HashMap;
use std::time::Duration;

use gpui_agent::Mode;
use serde_json::{json, Value};

#[test]
fn adapter_shape_failure_is_visible_and_escape_dismisses_from_the_field() {
    let mut harness = support::Fixture::new("add-tracks-error", 1, vec![])
        .with_source_fixture(luma_app::SourceAdapterFixture {
            library: json!({"trackCount": "not-a-number"}),
            playlists: json!([]),
            tracks: json!([]),
            playlist_tracks: HashMap::new(),
            searches: HashMap::new(),
        })
        .open(Mode::Headless);
    let script = support::script(
        r#"
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        const browser = until("the browser", (s) =>
            s.find({ role: "button", label: "Import tracks" }) !== undefined);
        app.click(browser.find({ role: "button", label: "Import tracks" }));
        const picker = until("the import-source menu", (s) =>
            s.find({ role: "row", label: "Rekordbox" }) !== undefined);
        app.click(picker.find({ role: "row", label: "Rekordbox" }));
        const failed = until("the source error", (s) =>
            s.find((n) => n.role === "text" && n.label.startsWith("Source error:")) !== undefined);
        const search = failed.find({ role: "input", label: "Search source…" });
        app.click(search);
        app.key("escape");
        const dismissed = until("Escape dismissal from source search", (s) =>
            s.find({ role: "button", label: "Close" }) === undefined);
        ({ error: failed.find((n) => n.role === "text" && n.label.startsWith("Source error:"))?.label,
           dismissed: dismissed.find({ role: "button", label: "Close" }) === undefined })
    "#,
    );
    let result = harness.exec(&script, Duration::from_secs(180));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    assert!(out["error"]
        .as_str()
        .is_some_and(|error| error.contains("invalid")));
    assert_eq!(out["dismissed"], true);
}
