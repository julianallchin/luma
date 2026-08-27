//! Empty all-Luma and empty normalized-source states through the real dialog.

#![cfg(feature = "app")]

use super::support;

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
        const picker = until("the import-source menu", (s) =>
            s.find({ role: "row", label: "Engine DJ" }) !== undefined);
        app.click(picker.find({ role: "row", label: "Engine DJ" }));
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

    drop(harness);
    let mut empty_source = support::Fixture::new("add-tracks-source-empty", 1, vec![])
        .without_track()
        .with_source_fixture(luma_app::SourceAdapterFixture {
            library: json!({"trackCount": 0}),
            playlists: json!([]),
            tracks: json!([]),
            playlist_tracks: HashMap::new(),
            searches: HashMap::new(),
        })
        .with_source_fixture_delay(Duration::from_millis(150))
        .open(Mode::Headless);
    let result = empty_source.exec(
        &support::script(
            r#"
            nav.venue("Test Venue");
            nav.step("the add-track affordance", "button", "Add track");
            const browser = until("the empty browser", (s) =>
                s.find({ role: "button", label: "Import tracks" }) ? s : undefined);
            app.click(browser.find({ role: "button", label: "Import tracks" }));
            const picker = until("the import-source menu", (s) =>
                s.find({ role: "row", label: "Rekordbox" }) ? s : undefined);
            app.click(picker.find({ role: "row", label: "Rekordbox" }));
            const loading = app.snapshot().find({ role: "text", label: "Loading source library…" }) !== undefined;
            const empty = until("the explicit empty source route", (s) =>
                s.find({ role: "text", label: "No source tracks" }) ? s : undefined);
            ({ loading, empty: empty.find({ role: "text", label: "No source tracks" }) !== undefined })
            "#,
        ),
        Duration::from_secs(180),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    assert_eq!(result.result["loading"], true);
    assert_eq!(result.result["empty"], true);
}
