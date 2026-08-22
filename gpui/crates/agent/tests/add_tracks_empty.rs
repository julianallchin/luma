//! Empty all-Luma and empty normalized-source states through the real dialog.

#![cfg(feature = "app")]

mod support;

use std::collections::HashMap;
use std::time::Duration;

use gpui_agent::Mode;
use serde_json::{json, Value};

#[test]
fn empty_library_centers_import_and_engine_source_normalizes_rows() {
    let mut harness = support::Fixture::new("add-tracks-empty", 1, vec![])
        .without_track()
        .with_source_fixture(luma_app::SourceAdapterFixture {
            library: json!({"databaseUuid": "engine-fixture", "trackCount": 1}),
            playlists: json!([{"id": 7, "title": "Engine crate", "parentId": null, "trackCount": 1}]),
            tracks: json!([{
                "id": 42,
                "path": "/fixture/engine.wav",
                "filename": "engine.wav",
                "title": "Engine normalized",
                "artist": "Engine artist",
                "album": null,
                "bpmAnalyzed": 124.0,
                "length": 180.0
            }]),
            playlist_tracks: HashMap::from([("7".into(), json!([{
                "id": 42,
                "path": "/fixture/engine.wav",
                "filename": "engine.wav",
                "title": "Engine normalized",
                "artist": "Engine artist",
                "album": null,
                "bpmAnalyzed": 124.0,
                "length": 180.0
            }]))]),
            searches: HashMap::new(),
        })
        .with_source_fixture_delay(Duration::from_millis(150))
        .open(Mode::Headless);
    let script = support::script(
        r#"
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        const empty = until("the empty all-Luma browser", (s) =>
            s.find({ role: "button", label: "Import tracks" }) !== undefined
                && s.findAll({ role: "row" }).length === 0);
        app.click(empty.find({ role: "button", label: "Import tracks" }));
        const picker = until("the source picker", (s) =>
            s.find({ role: "button", label: "Engine DJ" }) !== undefined);
        app.click(picker.find({ role: "button", label: "Engine DJ" }));
        const loading = app.snapshot().find((n) =>
            n.role === "text" && n.label === "Loading source library…") !== undefined;
        const engine = until("the normalized Engine source", (s) =>
            s.find({ role: "row", label: "Engine crate" }) !== undefined
                && s.find({ role: "row", label: "Engine normalized" }) !== undefined);
        ({ loading,
           empty: empty.findAll({ role: "row" }).length === 0,
           engineRow: engine.find({ role: "row", label: "Engine normalized" }) !== undefined,
           importEnabled: engine.find({ role: "button", label: "Import selected" })?.enabled })
    "#,
    );
    let result = harness.exec(&script, Duration::from_secs(180));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    assert_eq!(out["loading"], true);
    assert_eq!(out["empty"], true);
    assert_eq!(out["engineRow"], true);
    assert_eq!(out["importEnabled"], false);
}
