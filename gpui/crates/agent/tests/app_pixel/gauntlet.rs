//! The graph-editor gauntlet: the GPUI half of the reference captures in
//! `harness/gauntlet/`.
//!
//! `harness/gauntlet/shot-graph.mjs` renders the *web* pattern-graph canvas to
//! `web-<pattern>-<view>.png`; this is the same four shots out of the native
//! editor, written beside them as `gpui-<pattern>-<view>.png`, so the two
//! stacks can be put next to each other. `style-spec.md` is the measured
//! description of what those web shots contain and is the bar the GPUI canvas
//! is held to.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test gauntlet -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d because it is a *generator*, not an assertion: it creates a GPU
//! device, opens four windows the size of a graph, and overwrites files in the
//! repository. Nothing here fails on a pixel change — a human (or a critic)
//! compares the pairs.
//!
//! # Same graph, both stacks
//!
//! The fixtures under `harness/gauntlet/fixtures/` are the real saved graphs
//! for `gradient` and `circle_pill_step`, with positions normalized once
//! through the app's own `layoutGraph()` — see `extract-fixtures.ts` for why.
//! Both stacks read *that* file rather than the developer's library, so the
//! two pictures are provably of the same graph at the same coordinates. This
//! test seeds them into a throwaway library through
//! `save_pattern_graph_document`, because an authored graph document is the
//! only thing the editor can open (and the only thing it could write back).
//!
//! # Framing
//!
//! Neither view is driven by a gesture: the harness can click and drag, but it
//! cannot scroll, so a zoom cannot be typed in.
//!
//! - **whole** sizes the window so that the editor's own `fitView` lands at
//!   ≈0.5 — the zoom React Flow's `minZoom` pins the web whole-graph shot to.
//! - **closeup** sizes the window *past* the whole graph, so the same fit
//!   clamps at 1:1, and then crops the frame to a 900 × 460 window placed
//!   against a named card — the same region the web `closeup` viewport frames.
//!
//! Both are functions of the fixture and the window, so a rerun is the same
//! picture.

#![cfg(all(feature = "app", feature = "pixel"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{px, size, AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode};
use luma_lib::models::node_graph::Signal;
use serde_json::{json, Value};

/// The closeup crop, in logical pixels — the size of the web capture page's
/// canvas for the `closeup` view.
const CLOSEUP: (f32, f32) = (900., 460.);

/// One capture: which fixture, at what framing.
struct Shot {
    pattern: &'static str,
    view: &'static str,
    /// Window size in logical pixels. Chosen so the editor's fit lands where
    /// the module docs say it does.
    window: (f32, f32),
    /// `closeup` only: the card the crop is registered against, and where that
    /// card's top-left sits in the web capture's canvas. Picked as the
    /// left-most card with that title, because a graph may hold several of a
    /// type and only its position tells them apart.
    anchor: Option<(&'static str, f32, f32)>,
}

const SHOTS: &[Shot] = &[
    Shot {
        pattern: "gradient",
        view: "whole",
        // The gradient graph is ~1684 × 500; at 0.5 inside a 10%-padded
        // viewport that wants 1053px of canvas width, and the fit takes the
        // smaller of the two axes — so extra height only adds margin.
        window: (1053., 480.),
        anchor: None,
    },
    Shot {
        pattern: "gradient",
        view: "closeup",
        window: (2200., 800.),
        // `closeup: { x: -240, y: -30 }` puts graph (240, 30) at the canvas
        // origin, and `Linear Ramp` is at (320, 206).
        anchor: Some(("Linear Ramp", 80., 176.)),
    },
    Shot {
        pattern: "circle_pill_step",
        view: "whole",
        // ~3370 × 790, by the same arithmetic.
        window: (2106., 600.),
        anchor: None,
    },
    Shot {
        pattern: "circle_pill_step",
        view: "closeup",
        window: (4400., 1200.),
        // `closeup: { x: -260, y: -255 }`, and the left-most `Math` is at
        // (320, 285).
        anchor: Some(("Math", 60., 30.)),
    },
];

// -- the fixture --------------------------------------------------------------

/// The repository root, from this crate's manifest directory.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the repository root is above this crate")
}

/// Seed one pattern per fixture into a throwaway library, and give back the
/// directory it lives in.
///
/// Idempotency keys are derived from the pattern name so a re-seed of the same
/// directory replays rather than duplicates — the same discipline
/// `tests/graph.rs` keeps, for the same reason.
async fn seed(config_dir: &Path) {
    let db = luma_lib::database::local::database::init_app_db_at(config_dir)
        .await
        .expect("failed to open the fixture database");
    // A venue and a track, because the graph editor is not openable without a
    // track context (§6/§9 ruling 1 of the graph-editor design doc): the walk
    // below opens a track editor before it can open a pattern. Rows first,
    // while admission is still unarmed.
    let audio = config_dir.join("aurora.wav");
    std::fs::write(&audio, super::support::wav(8)).expect("failed to write the fixture audio");
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES (?, NULL, ?)")
        .bind(super::support::VENUE)
        .bind(super::support::VENUE_NAME)
        .execute(&db.0)
        .await
        .expect("failed to seed the venue");
    sqlx::query(
        "INSERT INTO tracks
            (id, uid, track_hash, title, artist, duration_seconds, file_path, created_at)
         VALUES (?, NULL, 'gauntlet-aurora-8s', ?, 'Nightliner', 8.0, ?, CURRENT_TIMESTAMP)",
    )
    .bind(super::support::TRACK)
    .bind(super::support::TRACK_NAME)
    .bind(audio.to_string_lossy().to_string())
    .execute(&db.0)
    .await
    .expect("failed to seed the track");
    let state_db = luma_lib::database::local::state::init_state_db_at(config_dir)
        .await
        .expect("failed to open the fixture state database");
    luma_lib::database::local::auth::bootstrap_headless_admission(&db.0, &state_db.0)
        .await
        .expect("failed to arm admission");
    let storage = luma_lib::storage::StorageRoot::from_path(config_dir.to_path_buf());
    let workspaces = Arc::new(
        luma_lib::agent_execution::workspace::PythonWorkspaceService::new(
            storage.agent_workspaces_dir(),
            Arc::new(|| Err("the fixture does not run Python workspaces".to_string())),
        ),
    );
    let services = luma_lib::dispatch::AppServices::headless(
        db,
        state_db,
        storage,
        config_dir.to_path_buf(),
        workspaces,
    );

    call(
        &services,
        "create_score",
        json!({
            "requestId": "7f1c2c60-0000-4000-8000-000000000300",
            "trackId": super::support::TRACK,
            "venueId": super::support::VENUE,
            "name": "Gauntlet Score",
        }),
    )
    .await;

    for (index, name) in ["gradient", "circle_pill_step"].into_iter().enumerate() {
        let fixture = fixture(name);
        let pattern = call(
            &services,
            "create_pattern",
            json!({
                "requestId": format!("7f1c2c60-0000-4000-8000-00000000010{index}"),
                "name": name,
                "description": null,
            }),
        )
        .await;
        let id = pattern["id"].as_str().expect("a created pattern has an id");
        let document = call(
            &services,
            "get_pattern_graph_document",
            json!({ "id": id, "implementationId": null }),
        )
        .await;
        call(
            &services,
            "save_pattern_graph_document",
            json!({
                "id": id,
                "implementationId": document["implementationId"],
                "operationId": format!("7f1c2c60-0000-4000-8000-00000000020{index}"),
                "baseRevision": document["revision"],
                "graph": fixture["graph"],
            }),
        )
        .await;
    }
}

/// One pattern's fixture: the graph both stacks render, the view-node signals
/// both stacks seed, and the closeup viewport.
fn fixture(name: &str) -> Value {
    let path = root().join(format!("harness/gauntlet/fixtures/{name}.json"));
    serde_json::from_slice(
        &std::fs::read(&path)
            .unwrap_or_else(|_| panic!("missing fixture for {name}; run extract-fixtures.ts")),
    )
    .expect("the fixture is not JSON")
}

async fn call(services: &luma_lib::dispatch::AppServices, name: &str, args: Value) -> Value {
    luma_lib::dispatch::dispatch(services, name, &args)
        .await
        .unwrap_or_else(|error| panic!("fixture command {name} failed: {error}"))
}

/// A library of its own, seeded once, named after the process so two runs
/// never share one.
fn fixture_config_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-gauntlet-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("failed to create the temporary config directory");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the fixture runtime")
        .block_on(seed(&dir));
    dir
}

fn harness(
    window: (f32, f32),
    views: HashMap<String, Signal>,
    config_dir: std::path::PathBuf,
) -> Harness {
    let root: gpui_agent::RootFactory = Arc::new(move |_: &mut Window, cx: &mut App| -> AnyView {
        luma_app::init(cx);
        // The same seeding the web capture page does with the same fixture
        // (`useViewDataStore.getState().setResults(fixture.viewSignals, …)`):
        // neither stack evaluates the graph for a screenshot, and a reference
        // shot with an empty 720px box in it is not a quality bar.
        luma_app::ViewData::publish(cx, views.clone());
        let library = luma_app::Library::open().expect("failed to open the fixture library");
        cx.new(|cx| luma_app::Luma::new(library, cx)).into()
    });
    Harness::headless(
        Config {
            mode: Mode::Pixel,
            window_size: size(px(window.0), px(window.1)),
            call_timeout: Duration::from_secs(60),
            runtime: luma_ui::runtime::Runtime {
                config_dir: Some(config_dir),
                ..luma_ui::runtime::Runtime::default()
            },
            ..Config::default()
        },
        root,
    )
    .expect("failed to start the harness")
}

// -- the capture --------------------------------------------------------------

/// Open the pattern, let the canvas settle, and hand back the frame plus the
/// anchor card's box.
///
/// `frames` is what waits for the graph document: the library is behind a
/// Tokio runtime gpui cannot see, and the fit needs a *measured* scene, which
/// is one more frame after that.
fn script(pattern: &str, anchor: Option<&str>) -> String {
    let anchor = match anchor {
        Some(title) => format!(
            r#"const cards = app.snapshot().findAll({{ role: "card", label: {title:?} }});
               if (cards.length === 0) throw new Error("no card titled {title}");
               anchor = cards.reduce((left, card) => card.bounds.x < left.bounds.x ? card : left).bounds;"#
        ),
        None => String::new(),
    };
    format!(
        r#"
        // The graph editor needs a track context (§6 of the graph-editor
        // design doc), so the door is venue → track → pattern; takeover and a
        // hidden sidebar give the canvas the window, as the old shell did.
        // The venue pick is conditional: the shots share one library and the
        // app remembers the venue, so only the first session sees the picker.
        {{
            const arrival = until("the venue picker or the shell", (s) =>
                (s.find({{ role: "card", label: "Test Venue" }})
                    || s.find({{ role: "input", label: "Search tracks…" }})) ? s : undefined);
            if (arrival.find({{ role: "card", label: "Test Venue" }})) {{
                nav.venue("Test Venue");
            }}
        }}
        nav.track("Aurora");
        nav.expand();
        app.key("secondary-b");
        app.frames(4);
        nav.pattern({pattern:?});
        app.frames(12);
        let anchor = null;
        {anchor}
        ({{ shot: app.screenshot(), anchor }})
    "#
    )
}

#[test]
#[ignore = "capture generator: needs a GPU and writes into harness/gauntlet"]
fn the_gauntlet_patterns_are_captured_from_the_native_editor() {
    let config_dir = fixture_config_dir();
    let out = root().join("harness/gauntlet");

    for shot in SHOTS {
        let views: HashMap<String, Signal> =
            serde_json::from_value(fixture(shot.pattern)["viewSignals"].clone())
                .expect("fixture viewSignals do not deserialize as Signals");
        let mut harness = harness(shot.window, views, config_dir.clone());
        let result = harness.exec(
            &super::support::script(&script(
                shot.pattern,
                shot.anchor.map(|(title, _, _)| title),
            )),
            Duration::from_secs(240),
        );
        assert_eq!(
            result.error, None,
            "{}/{} failed:\n{}",
            shot.pattern, shot.view, result.stdout
        );

        let captured = &result.result["shot"];
        let path = captured["path"].as_str().expect("a shot has a path");
        let width = captured["width"].as_u64().expect("a shot has a width") as f32;
        let mut image = image::open(path)
            .expect("the harness wrote a shot that is not an image")
            .to_rgba8();

        // The frame is physical pixels and every bound is logical, so one
        // scale converts the whole crop.
        let scale = width / shot.window.0;
        if let Some((_, offset_x, offset_y)) = shot.anchor {
            let anchor = &result.result["anchor"];
            let at = |key: &str| anchor[key].as_f64().expect("the anchor has a box") as f32;
            let crop = |value: f32| (value * scale).round().max(0.) as u32;
            image = image::imageops::crop_imm(
                &image,
                crop(at("x") - offset_x),
                crop(at("y") - offset_y),
                crop(CLOSEUP.0),
                crop(CLOSEUP.1),
            )
            .to_image();
        }

        let file = out.join(format!("gpui-{}-{}.png", shot.pattern, shot.view));
        image.save(&file).expect("failed to write the capture");
        println!("{}  {} x {}", file.display(), image.width(), image.height());
    }
}
