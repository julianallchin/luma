//! Frame-cost instruments for the add-track dialog routes. The gate on the
//! routes' picture is `tests/js/pixel/add_tracks.test.js`; these stay in Rust
//! because they print `support::cost` summaries and read environment knobs.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::collections::HashMap;
use std::time::Duration;

use gpui_agent::Mode;
use serde_json::{json, Value};
use support::Fixture;

fn run(harness: &mut gpui_agent::Harness, script: &str) -> Value {
    let result = harness.exec(&support::script(script), Duration::from_secs(180));
    assert_eq!(
        result.error, None,
        "pixel script failed:\n{}",
        result.stdout
    );
    result.result
}

// ---------------------------------------------------------------------------
// Frame cost
// ---------------------------------------------------------------------------

/// A source library of `count` tracks, so the import route is a list rather
/// than a line.
fn big_source_fixture(count: usize) -> luma_app::SourceAdapterFixture {
    let rows: Vec<Value> = (0..count)
        .map(|index| {
            json!({
                "id": format!("source-{index:05}"),
                "uuid": format!("source-uuid-{index:05}"),
                "filePath": format!("/fixture/source-{index:05}.wav"),
                "filename": format!("source-{index:05}.wav"),
                "title": format!("Source Track {index:05}"),
                "artist": format!("Source Artist {:03}", index % 89),
                "album": "Source Album",
                "bpm": 126.0,
                "durationSeconds": 180.0,
                "fileSize": 1024,
                "sampleRate": 44100
            })
        })
        .collect();
    luma_app::SourceAdapterFixture {
        library: json!({ "trackCount": count }),
        playlists: json!([{
            "id": "big-crate",
            "name": "Big crate",
            "parentId": null,
            "trackCount": count
        }]),
        tracks: json!(rows.clone()),
        playlist_tracks: HashMap::from([("big-crate".into(), json!(rows))]),
        searches: HashMap::new(),
    }
}

/// The measurement script. A placeholder rather than `format!` because the
/// body is JavaScript, and every brace in it would otherwise need doubling.
const SCRIPT: &str = r#"
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        until("the populated all-Luma route", (s) =>
            s.find({ role: "row", label: "__NEWEST__" }) !== undefined);
        app.frames(8, { waitMs: 16 });

        const browseFrom = app.frames(1).frame;
        app.frames(24, { waitMs: 16 });
        const browseTo = app.frames(1).frame;

        // The morph the user named: all-Luma browser -> source import. The
        // header chip only opens the source menu; picking a source is what
        // starts the route flight.
        app.click(app.snapshot().find({ role: "button", label: "Import tracks" }));
        const menu = until("the import-source menu", (s) =>
            s.find({ role: "row", label: "Rekordbox" })?.enabled === true ? s : undefined);
        // The mark comes off the snapshot itself: pumping a frame to read one
        // would make `menu` stale before the click.
        const flightFrom = menu.frame;
        app.click(menu.find({ role: "row", label: "Rekordbox" }));
        app.frames(12, { waitMs: 16 });
        const flightTo = app.frames(1).frame;
        app.frames(24, { waitMs: 16 });
        const sourceTo = app.frames(1).frame;
        ({ browseFrom, browseTo, flightFrom, flightTo, sourceTo,
           rows: app.snapshot().findAll({ role: "row" }).length,
           frames: app.timings().frames })
        "#;

/// Per-frame CPU cost of the route morph the lag report named: the all-Luma
/// browser to the source-import route, with a real library behind both.
///
/// `#[ignore]` because it is an instrument, not a gate.
#[test]
#[ignore = "measurement, not a gate"]
fn route_morph_frame_cost() {
    // Override with LUMA_COST_TRACKS to scale the signal on a busy machine.
    let tracks: usize = std::env::var("LUMA_COST_TRACKS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(600);
    // LUMA_COST_ART: unset = no art (the cheap arm), "on" = every row has a
    // real PNG, "broken" = every 8th row points at a file that is not there.
    let art = std::env::var("LUMA_COST_ART").unwrap_or_default();
    let mut builder = Fixture::new("add-tracks-cost", 8, vec![])
        .with_extra_tracks(tracks)
        .with_source_fixture(big_source_fixture(tracks))
        .with_motion()
        .window(1280.0, 800.0);
    builder = match art.as_str() {
        "on" => builder.with_album_art(0),
        "broken" => builder.with_album_art(8),
        _ => builder,
    };
    let mut fixture = builder.open(Mode::Pixel);
    // Newest first, and the list is virtualized: the row on screen is the
    // last one seeded, not the first.
    let newest = format!("Padding Track {:05}", tracks - 1);
    let out = run(&mut fixture, &SCRIPT.replace("__NEWEST__", &newest));
    let frames = out["frames"].as_array().unwrap();
    let mark = |key: &str| out[key].as_u64().unwrap();
    let art_label = match art.as_str() {
        "on" => "art",
        "broken" => "art, 1-in-8 missing",
        _ => "no art",
    };
    println!(
        "\n--- add-tracks route morph, {tracks} luma + {tracks} source tracks ({art_label}) ---"
    );
    println!("rows actually built into the tree: {}", out["rows"]);
    let browse = support::cost::summarize(
        frames,
        mark("browseFrom"),
        mark("browseTo"),
        "all-Luma route",
    );
    let flight = support::cost::summarize(
        frames,
        mark("flightFrom"),
        mark("flightTo"),
        "browse->import flight",
    );
    let source = support::cost::summarize(
        frames,
        mark("flightTo"),
        mark("sourceTo"),
        "source-import route",
    );
    println!(
        "the flight costs {:.2} ms/frame over the route it leaves",
        flight - browse
    );
    println!("and lands on a route costing {source:.2} ms/frame\n");
}

/// Wall-clock cost of a morph *while it is blurred*.
///
/// `drawMs` stops at the renderer's door (see `app.timings()`), and the one
/// thing a flight adds that a settled card never pays is
/// `Window::paint_filtered_layer`: an offscreen target plus a gaussian, per
/// non-identity layer, per frame. That is invisible to the CPU probe above, so
/// this one times a free-running pump instead — the only number here with the
/// GPU in it.
///
/// The flight is stretched so the pump samples its blurred stretch rather than
/// racing past it; `card` drops the filter the moment a pose is sharp and
/// unscaled, so at 1x most of a flight paints straight through.
#[test]
#[ignore = "measurement, not a gate"]
fn route_morph_gpu_cost() {
    const TRACKS: usize = 600;
    // LUMA_COST_WINDOW=WxH — the blur's cost is per pixel of the card plus its
    // sigma padding, so window size is the axis that matters here.
    let (width, height) = std::env::var("LUMA_COST_WINDOW")
        .ok()
        .and_then(|value| {
            let (w, h) = value.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1280.0, 800.0));
    let mut fixture = Fixture::new("add-tracks-gpu-cost", 8, vec![])
        .with_extra_tracks(TRACKS)
        .with_source_fixture(big_source_fixture(TRACKS))
        .with_motion()
        .with_motion_scale(40.0)
        .window(1280.0, 800.0)
        .open(Mode::Pixel);
    run(
        &mut fixture,
        r#"
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        until("the populated all-Luma route", (s) =>
            s.find({ role: "row", label: "Padding Track 00599" }) !== undefined);
        app.frames(8, { waitMs: 16 });
        ({})
        "#,
    );

    // Only `app.screenshot()` rasterizes. Pumping frames in this harness builds
    // scenes and never reaches the GPU, so a screenshot is the only way to make
    // the renderer do the work a real window does every frame — and therefore
    // the only way to see a content-filter pass at all. Run with
    // LUMA_FILTER_PROFILE=1 for the per-pass GPU numbers on stderr.
    run(
        &mut fixture,
        r#"
        // Settled: no filtered layer exists, so these are the card's floor.
        app.screenshot();
        app.screenshot();

        app.click(app.snapshot().find({ role: "button", label: "Import tracks" }));
        const menu = until("the import-source menu", (s) =>
            s.find({ role: "row", label: "Rekordbox" })?.enabled === true ? s : undefined);
        app.click(menu.find({ role: "row", label: "Rekordbox" }));

        // Early in the flight, where the pose still carries blur: `card` drops
        // the filter entirely once a layer is sharp and unscaled.
        for (let i = 0; i < 8; i += 1) {
            app.frames(1, { waitMs: 16 });
            app.screenshot();
        }
        ({})
        "#,
    );
    println!(
        "\n(FILTER_PROFILE lines are on stderr: filters=0 is the settled card, \
         filters>0 are blurred flight frames)\n"
    );
}

// ---------------------------------------------------------------------------
// The analysis view
// ---------------------------------------------------------------------------

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
    let mut harness = Fixture::new("add-tracks-analysis-pixels", 8, vec![])
        .with_source_fixture(luma_app::SourceAdapterFixture {
            library: json!({ "trackCount": 1 }),
            playlists: json!([]),
            tracks: json!([track]),
            playlist_tracks: HashMap::new(),
            searches: HashMap::new(),
        })
        .window(1000.0, 720.0)
        .open(Mode::Pixel);
    support::stub_genre_model(&support::config_dir("add-tracks-analysis-pixels"));

    let out = run(
        &mut harness,
        r#"
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
        "#,
    );
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
