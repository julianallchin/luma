//! Production pixel proof for the assembled add-track dialog routes.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
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

fn capture_dir() -> PathBuf {
    let dir = PathBuf::from(
        std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-add-tracks-review".into()),
    );
    fs::create_dir_all(&dir).expect("failed to create add-track capture directory");
    dir
}

fn preserve(source: &str, name: &str) -> PathBuf {
    let destination = capture_dir().join(name);
    fs::copy(source, &destination)
        .unwrap_or_else(|error| panic!("failed to preserve {}: {error}", destination.display()));
    println!("add-track capture {}", destination.display());
    destination
}

fn pixels(path: impl AsRef<Path>) -> image::RgbaImage {
    image::open(path.as_ref())
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.as_ref().display()))
        .to_rgba8()
}

/// The shared diff at the shared noise floor — see `support::image`.
fn differing_fraction(left: &image::RgbaImage, right: &image::RgbaImage) -> f32 {
    support::image::differing_fraction(left, right, support::image::CHANNEL_NOISE)
}

fn number(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("missing numeric {key}: {value:#}"))
}

fn width(bounds: &Value) -> f64 {
    number(bounds, "width")
}

fn height(bounds: &Value) -> f64 {
    number(bounds, "height")
}

fn source_fixture() -> luma_app::SourceAdapterFixture {
    let row = json!({
        "id": "pixel-source",
        "uuid": "pixel-source-uuid",
        "filePath": "/fixture/pixel-source.wav",
        "filename": "pixel-source.wav",
        "title": "Pixel Source Track",
        "artist": "Pixel Artist",
        "album": "Pixel Album",
        "bpm": 126.0,
        "durationSeconds": 180.0,
        "fileSize": 1024,
        "sampleRate": 44100
    });
    luma_app::SourceAdapterFixture {
        library: json!({"trackCount": 1}),
        playlists: json!([{
            "id": "pixel-crate",
            "name": "Pixel crate",
            "parentId": null,
            "trackCount": 1
        }]),
        tracks: json!([row.clone()]),
        playlist_tracks: HashMap::from([("pixel-crate".into(), json!([row]))]),
        searches: HashMap::new(),
    }
}

#[test]
fn production_routes_center_anchor_and_morph_without_clipping() {
    let mut empty = Fixture::new("add-tracks-pixels-empty", 1, vec![])
        .without_track()
        .window(1000.0, 720.0)
        .open(Mode::Pixel);
    let empty_out = run(
        &mut empty,
        r#"
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        // The import chip lives in the header on every route, so its presence
        // no longer means the list has finished loading — wait for the empty
        // body to name itself instead.
        const shot = until("the empty all-Luma route", (s) =>
            s.find({ role: "text", label: "No tracks in your library" })
                && s.findAll({ role: "row" }).length === 0 ? s : undefined);
        const card = shot.find({ role: "card", label: "Add tracks dialog" });
        const importButton = shot.find({ role: "button", label: "Import tracks" });
        const emptyLine = shot.find({ role: "text", label: "No tracks in your library" });
        ({ full: app.screenshot().path,
           cardShot: app.screenshot({ node: card }).path,
           card: card.bounds,
           importButton: importButton.bounds,
           emptyLine: emptyLine ? emptyLine.bounds : null })
        "#,
    );
    drop(empty);

    // An empty library is not a special layout. The palette keeps its bands,
    // the import affordance stays the header chip it is on every other route,
    // and the body says in words that it is empty — a centered hero button
    // would make "no tracks yet" look like a different screen from "one track".
    let empty_card = &empty_out["card"];
    let empty_button = &empty_out["importButton"];
    assert!(
        !empty_out["emptyLine"].is_null(),
        "the empty body did not name its own state: {empty_out:#}"
    );
    let header_gap = number(empty_button, "y") - number(empty_card, "y");
    assert!(
        (8.0..=24.0).contains(&header_gap),
        "empty import chip left the header band: gap={header_gap}, {empty_out:#}"
    );
    let empty_line = &empty_out["emptyLine"];
    assert!(
        number(empty_line, "y") > number(empty_button, "y") + height(empty_button),
        "the empty line is not below the header band: {empty_out:#}"
    );
    assert!(
        number(empty_line, "y") + height(empty_line)
            <= number(empty_card, "y") + height(empty_card),
        "the empty line escaped the card: {empty_out:#}"
    );
    preserve(empty_out["full"].as_str().unwrap(), "empty-browser.png");
    preserve(
        empty_out["cardShot"].as_str().unwrap(),
        "empty-browser-card.png",
    );

    let mut routes = Fixture::new("add-tracks-pixels-routes", 8, vec![])
        .with_source_fixture(source_fixture())
        .with_source_fixture_delay(Duration::from_millis(180))
        .with_motion()
        // Stretches the morph so before/mid/after samples land on distinct
        // frames on both 60Hz and 120Hz runners.
        .with_motion_scale(3.0)
        .window(1000.0, 720.0)
        .open(Mode::Pixel);
    let out = run(
        &mut routes,
        r#"
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        let shot = until("the populated all-Luma route", (s) =>
            s.find({ role: "row", label: "Aurora" })
                && s.find({ role: "button", label: "Import tracks" }) ? s : undefined);
        let card = shot.find({ role: "card", label: "Add tracks dialog" });
        const browser = app.screenshot().path;
        const browserBounds = card.bounds;
        const browserImportBounds = shot.find({ role: "button", label: "Import tracks" }).bounds;

        app.click(shot.find({ role: "button", label: "Import tracks" }));
        shot = until("the import-source menu", (s) =>
            s.find({ role: "row", label: "Rekordbox" })?.enabled === true ? s : undefined);
        card = shot.find({ role: "card", label: "Add tracks dialog" });
        const picker = app.screenshot().path;
        const pickerBounds = card.bounds;

        app.click(shot.find({ role: "row", label: "Rekordbox" }));
        // Mid-morph the palette must be showing BOTH routes' content — the
        // outgoing source choice and the incoming library — while the card
        // itself has not moved. That is the whole point of the one-size
        // palette: the frame is still, the content travels.
        shot = until("a real source-library morph frame", (s) =>
            s.find({ role: "row", label: "Rekordbox" })
                && s.find({ role: "input", label: "Search source…" }) ? s : undefined);
        card = shot.find({ role: "card", label: "Add tracks dialog" });
        const midpoint = app.screenshot().path;
        const midpointBounds = card.bounds;

        shot = until("the committed source library", (s) =>
            s.find({ role: "row", label: "Pixel Source Track" })
                && s.find({ role: "input", label: "Search source…" })?.focused === true ? s : undefined);
        card = shot.find({ role: "card", label: "Add tracks dialog" });
        const library = app.screenshot().path;
        const libraryBounds = card.bounds;
        const sourceImportBounds = shot.find({ role: "button", label: "Import selected" }).bounds;
        ({ browser, browserBounds, browserImportBounds,
           picker, pickerBounds,
           midpoint, midpointBounds,
           library, libraryBounds, sourceImportBounds })
        "#,
    );

    // Every route is the SAME palette. A picker that resized as you moved
    // through it would read as three dialogs taking turns; asserting one size
    // across all three (and across the morph frame between two of them) is
    // what holds that design in place.
    const PALETTE_WIDTH: f64 = 680.0;
    const PALETTE_HEIGHT: f64 = 416.0;
    for (label, bounds) in [
        ("browser", &out["browserBounds"]),
        ("picker", &out["pickerBounds"]),
        ("midpoint", &out["midpointBounds"]),
        ("library", &out["libraryBounds"]),
    ] {
        assert!(
            (width(bounds) - PALETTE_WIDTH).abs() <= 2.0
                && (height(bounds) - PALETTE_HEIGHT).abs() <= 2.0,
            "{label} is not the one palette size: {bounds:#}"
        );
        assert!(
            number(bounds, "x") >= 15.0
                && number(bounds, "y") >= 37.0
                && number(bounds, "x") + width(bounds) <= 985.0
                && number(bounds, "y") + height(bounds) <= 705.0,
            "{label} escaped the usable viewport: {bounds:#}"
        );
    }

    // The card must not travel across the route change either — the frame is
    // still and only the content moves. The tolerance covers `dialog_in`'s
    // 2px entrance rise, which can still be settling when the first route is
    // captured; anything larger is the palette actually relocating.
    for axis in ["x", "y"] {
        let drift =
            (number(&out["browserBounds"], axis) - number(&out["libraryBounds"], axis)).abs();
        assert!(
            drift <= 3.0,
            "the palette moved between routes on {axis} by {drift}"
        );
    }

    // The committing action rides in the HEADER band beside the key caps, not
    // in a footer: the footer is the key legend, and the chip is the one lit
    // key in the row above it.
    for (label, card, button) in [
        (
            "browser",
            &out["browserBounds"],
            &out["browserImportBounds"],
        ),
        ("source", &out["libraryBounds"], &out["sourceImportBounds"]),
    ] {
        let gap = number(button, "y") - number(card, "y");
        assert!(
            (8.0..=24.0).contains(&gap),
            "{label} submit chip is not seated in the header band: gap={gap}, {button:#}"
        );
        assert!(
            height(button) <= 24.0,
            "{label} submit chip is not at key-cap height: {button:#}"
        );
    }

    let browser = preserve(out["browser"].as_str().unwrap(), "track-browser.png");
    let picker = preserve(out["picker"].as_str().unwrap(), "source-picker.png");
    let midpoint = preserve(
        out["midpoint"].as_str().unwrap(),
        "source-morph-midpoint.png",
    );
    let library = preserve(out["library"].as_str().unwrap(), "source-library.png");
    assert!(
        differing_fraction(&pixels(&browser), &pixels(&picker)) > 0.08,
        "browser and picker are not visibly distinct production routes"
    );
    assert!(
        differing_fraction(&pixels(&midpoint), &pixels(&library)) > 0.04,
        "morph midpoint is not visibly distinct from its committed route"
    );
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
