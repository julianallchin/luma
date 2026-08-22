//! Source row intent is one generation across all/search/playlist requests.

#![cfg(feature = "app")]

mod support;

use std::collections::HashMap;
use std::time::Duration;

use gpui_agent::Mode;
use serde_json::{json, Value};

fn row(id: &str, title: &str) -> Value {
    json!({
        "uuid": id,
        "filePath": null,
        "filename": format!("{id}.wav"),
        "title": title,
        "artist": null,
        "album": null,
        "bpm": null,
        "durationSeconds": null
    })
}

#[test]
fn repeated_query_identity_cannot_admit_an_older_response() {
    let mut harness = support::Fixture::new("add-tracks-source-race", 1, vec![])
        .with_source_fixture(luma_app::SourceAdapterFixture {
            library: json!({"trackCount": 1}),
            playlists: json!([{
                "id": "crate",
                "name": "Crate",
                "parentId": null,
                "trackCount": 1
            }]),
            tracks: json!([row("all", "All row")]),
            playlist_tracks: HashMap::from([(
                "crate".into(),
                json!([row("playlist", "Playlist row")]),
            )]),
            searches: HashMap::from([("a".into(), json!([row("global-a", "Global A")]))]),
        })
        .with_source_search_responses(vec![
            luma_app::SourceSearchFixtureResponse {
                query: "a".into(),
                delay: Duration::from_millis(300),
                rows: json!([row("stale-a", "Stale A")]),
            },
            luma_app::SourceSearchFixtureResponse {
                query: "ab".into(),
                delay: Duration::from_millis(20),
                rows: json!([row("ab", "AB")]),
            },
            luma_app::SourceSearchFixtureResponse {
                query: "a".into(),
                delay: Duration::from_millis(60),
                rows: json!([row("current-a", "Current A")]),
            },
        ])
        .open(Mode::Headless);
    let script = support::script(
        r#"
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        const browser = until("the browser", (s) =>
            s.find({ role: "button", label: "Import tracks" }) !== undefined);
        app.click(browser.find({ role: "button", label: "Import tracks" }));
        const picker = until("the source picker", (s) =>
            s.find({ role: "button", label: "Rekordbox" }) !== undefined);
        app.click(picker.find({ role: "button", label: "Rekordbox" }));
        const source = until("the source", (s) =>
            s.find({ role: "row", label: "All row" }) !== undefined);
        app.click(source.find({ role: "input", label: "Search source…" }));
        app.key("a");
        app.key("b");
        app.key("backspace");
        const current = until("the latest repeated A intent", (s) =>
            s.find({ role: "row", label: "Current A" }) !== undefined);
        app.frames(40, { waitMs: 10 });
        const afterStale = app.snapshot();

        app.click(afterStale.find({ role: "row", label: "Crate" }));
        const playlist = until("playlist scope clears search", (s) =>
            s.find({ role: "row", label: "Playlist row" }) !== undefined);
        app.click(playlist.find({ role: "input", label: "Search source…" }));
        app.key("a");
        const searched = until("search scope clears playlist", (s) =>
            s.find({ role: "row", label: "Global A" }) !== undefined);

        ({ current: current.find({ role: "row", label: "Current A" }) !== undefined,
           staleRejected: afterStale.find({ role: "row", label: "Current A" }) !== undefined
               && afterStale.find({ role: "row", label: "Stale A" }) === undefined,
           playlist: playlist.find({ role: "row", label: "Playlist row" }) !== undefined,
           search: searched.find({ role: "row", label: "Global A" }) !== undefined })
    "#,
    );
    let result = harness.exec(&script, Duration::from_secs(180));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    for key in ["current", "staleRejected", "playlist", "search"] {
        assert_eq!(
            out[key], true,
            "source generation contract failed at {key}: {out:#}"
        );
    }
}
