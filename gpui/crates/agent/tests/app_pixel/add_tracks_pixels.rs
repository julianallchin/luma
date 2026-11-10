//! The add-track dialog's analysis view under a real renderer. The gate on the
//! routes' picture is `tests/js/pixel/add_tracks.test.js`.

use super::support;

use std::time::Duration;

use gpui_agent::Mode;
use serde_json::json;
use support::Fixture;

/// The progress page mid-analysis and after it, under a real renderer: the
/// row names the stages it is on over a stage-count bar while the header
/// still says analysing, and both finish together.
///
/// In Rust rather than a script because the fake analysis has to be on disk
/// (`LUMA_CACHE_DIR`) before the library opens.
#[cfg(unix)]
#[test]
fn analysis_view_mid_analysis() {
    let _environment = support::environment_lock();
    let external = std::env::temp_dir().join(format!(
        "luma-add-tracks-analysis-audio-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&external).ok();
    std::fs::create_dir_all(&external).unwrap();
    let audio = external.join("needle.wav");
    std::fs::write(&audio, support::wav(4)).unwrap();
    let cache = external.join("cache");
    // Slow enough that a frame mid-analysis holds still for the capture.
    support::install_fake_analysis(&cache, 1.5);
    std::env::set_var("LUMA_CACHE_DIR", &cache);

    let track = json!({
        "id": "rb-content",
        "uuid": "rb-needle",
        "filePath": audio,
        "filename": "needle.wav",
        "title": "Needle",
        "artist": "Artist",
        "album": "Album",
        "bpm": 128.0,
        "durationSeconds": 4.0,
        "fileSize": std::fs::metadata(&audio).unwrap().len(),
        "sampleRate": 44100,
    });
    let mut harness = Fixture::from_json(
        "add-tracks-analysis-pixels",
        json!({
            "source_fixture": {
                "library": { "trackCount": 1 },
                "playlists": [],
                "tracks": [track],
            },
            "window": [1000.0, 720.0],
        }),
    )
    .unwrap()
    .open(Mode::Pixel);
    support::stub_genre_model(&support::config_dir("add-tracks-analysis-pixels"));

    let out = harness.run(&support::script(r#"
        const card = (s) => s.find({ role: "card", label: "Add tracks dialog" });
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        let s = until("the browser", (s) => s.find({ role: "button", label: "Import tracks" }) ? s : undefined);
        app.click(s.find({ role: "button", label: "Import tracks" }));
        s = until("the source menu", (s) => s.find({ role: "row", label: "Rekordbox" })?.enabled ? s : undefined);
        app.click(s.find({ role: "row", label: "Rekordbox" }));
        s = until("the source rows", (s) => s.find({ role: "row", label: "Needle" }) ? s : undefined);
        app.click(s.find({ role: "row", label: "Needle" }));
        s = until("the enabled import", (s) => s.find({ role: "button", label: "Import selected" })?.enabled ? s : undefined);
        app.click(s.find({ role: "button", label: "Import selected" }));

        const row = (s) => s.findAll({ role: "row" }).find((n) => n.label.startsWith("Needle: "));
        // A stage running and at least one settled, so the bar has a fill.
        const mid = until("a stage running past the first", (s) =>
            /^Needle: Analyzing · .* \([1-9]\d* of \d+\)$/.test(row(s)?.label ?? "") ? s : undefined,
            { timeoutMs: 30000 });
        app.frames(2, { waitMs: 30 });
        const midShot = app.screenshot({ node: card(app.snapshot()) });
        image.keep(midShot, "add-tracks/analysis-mid");
        const midHeader = mid.findAll({ role: "text" }).find((n) => n.label.startsWith("Analyzing"))?.label;

        const done = until("the import finished", (s) => {
            const label = row(s)?.label;
            return label === "Needle: Ready" || label === "Needle: Failed" ? s : undefined;
        }, { timeoutMs: 90000 });
        app.frames(2, { waitMs: 30 });
        const doneShot = app.screenshot({ node: card(app.snapshot()) });
        image.keep(doneShot, "add-tracks/analysis-done");
        const doneHeader = done.findAll({ role: "text" })
            .find((n) => /^(Imported|\d+ of \d+ failed)/.test(n.label))?.label;
        ({ midRow: row(mid).label, midHeader, doneRow: row(done).label, doneHeader,
           changed: image.diff(midShot, doneShot) })
        "#), Duration::from_secs(180));
    std::env::remove_var("LUMA_CACHE_DIR");
    println!("analysis view: {out}");
    assert_eq!(out["midHeader"], "Analyzing 1 song…", "{out}");
    assert!(
        out["doneHeader"].is_string(),
        "the header never finished: {out}"
    );
    // The bar and the stage words are gone once the row is: the two pictures
    // differ by more than the header's words.
    assert!(out["changed"].as_f64().unwrap() > 0.001, "{out}");
}
