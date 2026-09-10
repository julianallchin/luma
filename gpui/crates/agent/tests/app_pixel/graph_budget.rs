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

use super::support::{self, Fixture};
use gpui_agent::{Harness, Mode};
use serde_json::{json, Value};
use std::time::Duration;

const COLUMNS: usize = 12;
const ROWS: usize = 10;
const NODE_COUNT: usize = COLUMNS * ROWS + 1;
const BUDGET_MS: f64 = 8.33;

fn harness() -> Harness {
    let mut nodes = serde_json::Map::new();
    for row in 0..ROWS {
        for column in 0..COLUMNS {
            let input = if column == 0 {
                json!({"source":"value","value":{"type":"proportion","value":0.5}})
            } else {
                json!({"source":"connection","node":format!("{row}-{}",column-1),"output":"value"})
            };
            nodes.insert(
                format!("{row}-{column}"),
                json!({"definition":"core/floor",
                "position":[column*240,row*140],"inputs":{"value":input}}),
            );
        }
    }
    nodes.insert("output".into(),json!({"definition":"output","position":[COLUMNS*240,0],
        "inputs":{"dimmer":{"source":"connection","node":format!("0-{}",COLUMNS-1),"output":"value"}}}));
    Fixture::new("graph-budget",8,vec![]).with_graph_score(json!({
        "version":3,"definitions":{"budget":{"name":"Budget Graph","inputs":{},
            "outputs":{"lighting":{"value_type":"lighting","rate":"frame"}},
            "body":{"kind":"graph","body":{"nodes":nodes,
                "outputs":{"lighting":{"source":"connection","node":"output","output":"lighting"}}}}
        }},"clips":{"clip":{"graph":"budget","start":0,"duration":8,"seed":0}}
    })).with_rig().open(Mode::Pixel)
}

/// Open the graph, then pan and zoom as continuous gestures — steady-state
/// frames, exactly as `track_editor_budget` measures its legs.
const SCRIPT: &str = r#"
    nav.trackEditor("Test Venue", "Aurora");
    nav.expand();nav.stageOff();nav.pattern("Budget Graph");
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
        return app.snapshot().findAll({ role: "card", label: "Floor" });
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
