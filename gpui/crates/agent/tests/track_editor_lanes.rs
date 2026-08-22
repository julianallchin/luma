//! The lane stack's vertical contract, on a score with more layers than the
//! canvas is tall.
//!
//! Everything here is the half of `harness/gauntlet-te/behavior-spec.md` §0 and
//! §4 that only shows up once the lanes overflow: the block is bottom-anchored,
//! so z = 0 — the layer everything else is stacked over — sits on the floor and
//! the lanes that do not fit run off the *top*, under the waveform. The two
//! ways back to them are the bare wheel and the vertical zoom, and `H` is the
//! one that fits the whole stack at once.
//!
//! A separate binary from `track_editor_ux` because `support::Fixture` sets
//! `LUMA_CONFIG_DIR`, which is process-global: one fixture per process, and
//! this contract needs a score the other one's assertions could not survive.

#![cfg(feature = "app")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 20;

/// Eight layers, one clip each. Nine lanes with the empty insertion lane, or
/// 832 pixels at the default lane height — more than the window has, which is
/// the whole point of the fixture.
const LAYERS: usize = 8;

/// `TRACK_HEIGHT`, which is what a lane measures at `zoomY == 1`.
const LANE: f64 = 80.;

fn harness() -> Harness {
    Fixture::new(
        "track-editor-lanes",
        TRACK_SECONDS,
        (0..LAYERS)
            .map(|z| Clip::new(format!("pattern-l{z}"), format!("L{z}"), 2., 6.).lane(z as i64))
            .collect(),
    )
    .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    function shot() { return app.snapshot(); }
    function node(role, label) { return shot().find({ role, label }); }

    /** Every lane, top to bottom, with the height it is *visible* at — a lane
        the waveform covers is clipped to nothing, which is the harness's way
        of saying no pointer could reach it. */
    function lanes() {
        return shot()
            .findAll({ role: "row" })
            .filter((n) => n.label.startsWith("Lane "))
            .map((n) => ({ label: n.label, y: n.bounds.y, height: n.bounds.height }))
            .sort((a, b) => a.y - b.y);
    }

    /** The canvas's own extent: the playhead spans the whole of it. */
    function canvas() {
        const head = node("slider", "Playhead");
        return { top: head.bounds.y, bottom: head.bounds.y + head.bounds.height };
    }

    function reading() {
        const rows = lanes();
        return {
            lanes: rows,
            first: rows[0],
            last: rows[rows.length - 1],
            canvas: canvas(),
            shortest: Math.min(...rows.map((r) => r.height)),
            tallest: Math.max(...rows.map((r) => r.height)),
        };
    }

    nav.venue("Test Venue");
    app.frames(8);
    nav.track("Aurora");
    // Waited for by its result (the timeline's waveform card), not by a
    // frame count: nav.track returns as soon as the row was pressable,
    // which is earlier than the old hand-rolled walk got here, so a bare
    // frames(20) can land inside the load.
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
        nav.expand();

    const opened = reading();

    // The bare wheel is the scroll container's: a vertical notch over the lanes
    // walks the block up off the floor, and walking it back down lands where it
    // started however far past the end it is pushed.
    app.scroll(node("row", "Lane 5"), { dy: 400, steps: 10 });
    app.frames(2);
    const lifted = reading();
    app.scroll(node("row", "Lane 5"), { dy: -4000, steps: 20 });
    app.frames(2);
    const dropped = reading();

    // Alt+wheel is the vertical zoom, clamped at both ends.
    app.scroll(node("row", "Lane 5"), { dy: -600, steps: 10, modifiers: ["alt"] });
    app.frames(2);
    const shrunk = reading();

    // Over the waveform it means nothing — that band is a fixed navigation
    // surface, not part of the workspace.
    app.scroll(node("card", "Waveform"), { dy: 600, steps: 10, modifiers: ["alt"] });
    app.frames(2);
    const ignored = reading();

    app.scroll(node("row", "Lane 5"), { dy: 600, steps: 10, modifiers: ["alt"] });
    app.frames(2);
    const grown = reading();

    // H fits the stack: every lane on the canvas at once, still on the floor.
    app.key("h");
    app.frames(2);
    const fitted = reading();

    ({ opened, lifted, dropped, shrunk, ignored, grown, fitted })
"#;

#[test]
fn the_lane_stack_sits_on_the_floor_and_the_wheel_reaches_the_rest_of_it() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // 0. Nine lanes: one per layer, plus the empty insertion lane above them.
    let opened = &out["opened"];
    assert_eq!(
        opened["lanes"].as_array().map(Vec::len),
        Some(LAYERS + 1),
        "the fixture did not open on {LAYERS} layers: {opened:#}"
    );

    // 1. Bottom-anchored: the lowest lane's floor is the canvas's, and the
    //    lanes that do not fit are the ones off the *top*.
    on_the_floor(opened, "the lanes as they opened");
    assert_eq!(
        number(&opened["first"], "height"),
        0.,
        "the overflow should run off the top, under the waveform: {opened:#}"
    );

    // 2. A bare vertical wheel walks the block up, which is the only way to
    //    reach a lane the overflow hid — and walking it back down puts z = 0
    //    on the floor again rather than scrolling past it.
    let lifted = &out["lifted"];
    assert!(
        number(&lifted["first"], "height") > 0.,
        "a vertical wheel over the lanes moved nothing: {opened:#} -> {lifted:#}"
    );
    let dropped = &out["dropped"];
    assert_eq!(
        (
            number(&dropped["first"], "height"),
            number(&dropped["last"], "y")
        ),
        (
            number(&opened["first"], "height"),
            number(&opened["last"], "y")
        ),
        "scrolling back down did not land on the floor: {dropped:#}"
    );
    on_the_floor(dropped, "the lanes after scrolling back down");

    // 3. Alt+wheel zooms the lanes, and clamps at `MIN_ZOOM_Y` / `MAX_ZOOM_Y`
    //    — half a lane and half again, both measured off the same constant.
    let shrunk = &out["shrunk"];
    assert_eq!(
        number(shrunk, "tallest"),
        LANE / 2.,
        "alt-wheel did not shrink the lanes to MIN_ZOOM_Y: {shrunk:#}"
    );
    assert_eq!(
        number(shrunk, "shortest"),
        LANE / 2.,
        "at half height every lane fits, so none of them should be clipped: {shrunk:#}"
    );
    on_the_floor(shrunk, "the shrunk lanes");
    assert_eq!(
        out["ignored"]["tallest"], shrunk["tallest"],
        "an alt-wheel over the waveform zoomed the lanes: {:#}",
        out["ignored"]
    );
    // Growing them again is not asserted back onto the floor: the anchor is
    // rows-from-the-floor under the pointer, and a stack that fitted the canvas
    // had no scroll to hold it with — so the notch that grows past the viewport
    // starts from wherever that clamp left it, exactly as the web's does.
    let grown = &out["grown"];
    assert_eq!(
        number(grown, "tallest"),
        LANE * 1.5,
        "alt-wheel did not grow the lanes to MAX_ZOOM_Y: {grown:#}"
    );

    // 4. `H` fits the stack: nothing clipped away, and still anchored.
    let fitted = &out["fitted"];
    let canvas = number(&fitted["canvas"], "bottom") - number(&fitted["canvas"], "top");
    let want = ((canvas - 112.) / (LAYERS + 1) as f64).floor();
    assert!(
        number(fitted, "shortest") >= want - 2.,
        "H left a lane clipped out of view; every one of them should fit: {fitted:#}"
    );
    on_the_floor(fitted, "the fitted lanes");
}

/// z = 0 is on the canvas floor, which is the whole of what bottom-anchored
/// means and the one thing every reading here shares.
fn on_the_floor(reading: &Value, what: &str) {
    let floor = number(&reading["last"], "y") + number(&reading["last"], "height");
    let canvas = number(&reading["canvas"], "bottom");
    assert!(
        (floor - canvas).abs() <= 1.,
        "{what}: the lowest lane ends at {floor}, not on the canvas floor at {canvas}: {reading:#}"
    );
}

fn number(reading: &Value, key: &str) -> f64 {
    reading[key]
        .as_f64()
        .unwrap_or_else(|| panic!("no {key} in {reading:#}"))
}
