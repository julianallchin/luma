//! What one frame of the graph editor costs while the eye is moving.
//!
//! The routing architecture (`docs/design/graph-editor-interaction.md` §2)
//! was chosen so that pan and zoom cost one repaint of one element — no
//! layout, no re-measure, no per-frame hit-tree work. This test is that
//! contract turned into a number: a graph an order of magnitude past today's
//! real ones, panned and zoomed continuously while every frame in between is
//! timed. It lands with the first interaction change on purpose — the budget
//! is what the hit-tree and the marquee are not allowed to spend.
//!
//! # Why pixel mode
//!
//! Headless mode's text system returns invented metrics, so every card title
//! and port label shapes for free there. Shaping is the dominant per-glyph
//! cost on this canvas (`graph.rs` module docs), so a headless number would
//! exclude exactly the cost most likely to regress. Pixel mode is the same
//! deterministic platform with the real text system plugged in.
//!
//! As with `track_editor_budget`, these are CPU frame times — event handling
//! plus the layout/prepaint/paint walk — because the pinned gpui rev has no
//! public entry point for GPU timings (`app.timings()`).
//!
//! # Why it is `#[ignore]`
//!
//! It creates a GPU device and asserts a wall-clock percentile, so it is a
//! measurement, not a gate — a loaded CI box would fail it for reasons that
//! have nothing to do with the code. Run it on demand:
//!
//! ```sh
//! CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel \
//!     --test app_pixel graph_budget -- --ignored --nocapture
//! ```

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode};
use serde_json::{json, Value};

/// Idempotency keys, as in `headless/graph.rs`: fixed UUIDs so re-seeding a
/// directory replays rather than duplicates.
const REQUEST_ID: &str = "7f1c2c61-0000-4000-8000-000000000001";
const OPERATION_ID: &str = "7f1c2c61-0000-4000-8000-000000000002";
const SCORE_REQUEST_ID: &str = "7f1c2c61-0000-4000-8000-000000000003";

const PATTERN: &str = "Budget Graph";

/// A 12 × 10 grid — 120 nodes, chained along each row. Real patterns run a
/// dozen nodes; the budget is taken an order of magnitude past that so the
/// number moves before anyone feels it.
const COLUMNS: usize = 12;
const ROWS: usize = 10;
const NODE_COUNT: usize = COLUMNS * ROWS;

/// 120 Hz — the same bar `track_editor_budget` holds the timeline to.
const BUDGET_MS: f64 = 8.33;

/// Build a pattern whose graph is `NODE_COUNT` `round` nodes on a grid, each
/// row chained `out -> in`. `round` on purpose: its card carries a selector,
/// so the measure pass and the paint both do real per-card work (a ghost
/// stack of shaped options), which is what a busy real graph looks like.
async fn seed(config_dir: &Path) -> String {
    let db = luma_lib::database::local::database::init_app_db_at(config_dir)
        .await
        .expect("failed to open the fixture database");
    let audio = config_dir.join("aurora.wav");
    std::fs::write(&audio, support::wav(8)).expect("failed to write the fixture audio");
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES (?, NULL, ?)")
        .bind(support::VENUE)
        .bind(support::VENUE_NAME)
        .execute(&db.0)
        .await
        .expect("failed to seed the venue");
    sqlx::query(
        "INSERT INTO tracks
            (id, uid, track_hash, title, artist, duration_seconds, file_path, created_at)
         VALUES (?, NULL, 'graph-aurora-8s', ?, 'Nightliner', 8.0, ?, CURRENT_TIMESTAMP)",
    )
    .bind(support::TRACK)
    .bind(support::TRACK_NAME)
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
            "requestId": SCORE_REQUEST_ID,
            "trackId": support::TRACK,
            "venueId": support::VENUE,
            "name": "Fixture Score",
        }),
    )
    .await;

    let pattern = call(
        &services,
        "create_pattern",
        json!({ "requestId": REQUEST_ID, "name": PATTERN, "description": null }),
    )
    .await;
    let id = pattern["id"].as_str().expect("a created pattern has an id");
    let document = call(
        &services,
        "get_pattern_graph_document",
        json!({ "id": id, "implementationId": null }),
    )
    .await;

    let nodes: Vec<Value> = (0..NODE_COUNT)
        .map(|index| {
            json!({
                "id": format!("r{index}"),
                "typeId": "round",
                "params": {},
                "positionX": (index % COLUMNS) as f64 * 260.0,
                "positionY": (index / COLUMNS) as f64 * 220.0,
            })
        })
        .collect();
    let edges: Vec<Value> = (0..NODE_COUNT)
        .filter(|index| (index + 1) % COLUMNS != 0)
        .map(|index| {
            json!({
                "id": format!("e{index}"),
                "fromNode": format!("r{index}"),
                "fromPort": "out",
                "toNode": format!("r{}", index + 1),
                "toPort": "in",
            })
        })
        .collect();
    call(
        &services,
        "save_pattern_graph_document",
        json!({
            "id": id,
            "implementationId": document["implementationId"],
            "operationId": OPERATION_ID,
            "baseRevision": document["revision"],
            "graph": { "nodes": nodes, "edges": edges, "args": [] },
        }),
    )
    .await;

    id.to_string()
}

async fn call(services: &luma_lib::dispatch::AppServices, name: &str, args: Value) -> Value {
    luma_lib::dispatch::dispatch(services, name, &args)
        .await
        .unwrap_or_else(|error| panic!("fixture command {name} failed: {error}"))
}

fn fixture_config_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-graph-budget-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("failed to create the temporary config directory");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the fixture runtime")
        .block_on(seed(&dir));
    dir
}

fn harness() -> Harness {
    let config_dir = fixture_config_dir();
    let root: gpui_agent::RootFactory = Arc::new(|_: &mut Window, cx: &mut App| -> AnyView {
        luma_app::init(cx);
        let library = luma_app::Library::open().expect("failed to open the fixture library");
        cx.new(|cx| luma_app::Luma::new(library, cx)).into()
    });
    Harness::headless(
        Config {
            mode: Mode::Pixel,
            call_timeout: Duration::from_secs(60),
            runtime: support::runtime(config_dir),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start the harness")
}

/// Open the graph, then pan and zoom as continuous gestures — steady-state
/// frames, exactly as `track_editor_budget` measures its legs.
const SCRIPT: &str = r#"
    nav.trackEditor("Test Venue", "Aurora");
    nav.patterns();
    app.frames(6);
    nav.step("the pattern Budget Graph", "row", "Budget Graph");
    nav.expand();
    app.frames(8);

    /** Every frame drawn while `run` ran, as total CPU milliseconds. */
    function measure(run) {
        const from = app.frames(1).frame;
        run();
        return app
            .timings()
            .frames.filter((f) => f.frame > from)
            .map((f) => ({ total: f.parkedMs + f.drawMs, draw: f.drawMs }));
    }

    function graphCards() {
        return app.snapshot().findAll({ role: "card", label: "Round" });
    }

    const cards = graphCards();
    const left = Math.min(...cards.map((c) => c.bounds.x));
    const top = Math.min(...cards.map((c) => c.bounds.y));

    // Panning: a plain drag from empty ground just outside the fitted
    // graph's corner. The fit pads 10% of the canvas, so the corner is
    // canvas, not chrome.
    const before = cards.map((c) => c.bounds.x);
    const pan = measure(() =>
        app.drag({ x: left - 20, y: top - 20 }, { dx: 300, dy: 200, steps: 60 }),
    );
    const moved = graphCards().map((c) => c.bounds.x);

    // Zooming: the wheel over a card, out and back in, each step a frame.
    const anchor = graphCards()[0];
    const zoom = measure(() => {
        app.scroll(anchor, { dy: -400, steps: 30 });
        app.scroll(anchor, { dy: 400, steps: 30, restale: "match" });
    });

    ({
        pan,
        zoom,
        moved: moved.some((x, i) => x !== before[i]),
        status: app.snapshot().findAll({ role: "text" })
            .map((n) => n.label)
            .find((l) => l.endsWith("NODES")),
        mode: app.timings().mode,
    })
"#;

#[test]
#[ignore = "measures wall-clock frame times on a GPU device; run on demand"]
fn panning_and_zooming_a_hundred_node_graph_stays_inside_the_frame_budget() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    assert_eq!(
        out["mode"], "pixel",
        "these numbers are not from pixel mode"
    );
    assert_eq!(
        out["status"],
        json!(format!("{NODE_COUNT} NODES")),
        "the editor did not open the seeded graph: {out:#}"
    );
    assert_eq!(
        out["moved"],
        Value::Bool(true),
        "the pan leg drew sixty frames of a canvas that never moved"
    );

    let pan = Leg::read(&out["pan"], "pan");
    let zoom = Leg::read(&out["zoom"], "zoom");
    println!("\n{pan}\n{zoom}\n");

    for leg in [&pan, &zoom] {
        assert!(
            leg.total_p95 <= BUDGET_MS,
            "{} p95 is {:.2} ms, over the {BUDGET_MS} ms budget\n{leg}",
            leg.name,
            leg.total_p95,
        );
    }
}

/// One continuous gesture's frames — the same reading `track_editor_budget`
/// takes, minus its web baseline (no web graph capture exists to compare to).
struct Leg {
    name: &'static str,
    total: Vec<f64>,
    draw: Vec<f64>,
    total_p95: f64,
}

impl Leg {
    fn read(frames: &Value, name: &'static str) -> Self {
        let frames = frames
            .as_array()
            .unwrap_or_else(|| panic!("{name} produced no frames: {frames:#}"));
        assert!(
            frames.len() >= 30,
            "{name} drew only {} frames, too few to take a percentile of",
            frames.len()
        );
        let field = |key: &str| -> Vec<f64> {
            frames
                .iter()
                .map(|frame| frame[key].as_f64().unwrap_or(f64::NAN))
                .collect()
        };
        let total = field("total");
        Self {
            total_p95: p95(&total),
            total,
            draw: field("draw"),
            name,
        }
    }
}

impl std::fmt::Display for Leg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:>6}  {:>3} frames   p50 {:>6.2}  p95 {:>6.2}  max {:>6.2} ms   (draw p95 {:>6.2} ms)",
            self.name,
            self.total.len(),
            p50(&self.total),
            self.total_p95,
            self.total.iter().copied().fold(0., f64::max),
            p95(&self.draw),
        )
    }
}

/// Nearest-rank percentile — see `track_editor_budget` for why there is no
/// interpolation: the question is whether a real frame missed the deadline.
fn percentile(samples: &[f64], q: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

fn p50(samples: &[f64]) -> f64 {
    percentile(samples, 0.5)
}

fn p95(samples: &[f64]) -> f64 {
    percentile(samples, 0.95)
}
