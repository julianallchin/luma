//! The track editor's pointer and view contract, driven through real input.
//!
//! Everything here is a behavior the web timeline has and this canvas had to
//! grow: which vertical band answers a press, that only a clip's header bar is
//! grabbable, that a sweep of empty lane selects what it *contains*, that a
//! resize snaps to the beat grid and moves every selected clip, and that the
//! wheel scrolls where it is not zooming. The contract they are checked
//! against is `harness/gauntlet-te/behavior-spec.md`, which is the web source
//! turned into an index.
//!
//! # Why one test and not one per cluster
//!
//! [`support::Fixture`] seeds a library and points `LUMA_CONFIG_DIR` at it,
//! and that variable is process-global — two tests in this binary would be one
//! library with both their contents, racing on the same directory. So the
//! clusters are sections of one script and one set of assertions, each named
//! for what it is proving, and a second cluster's failure does not hide behind
//! the first because every section reports its own reading.
//!
//! # Why the readings are geometry and toolbar text together
//!
//! A clip's node bounds say where the canvas drew it; the toolbar says how
//! many clips are selected and what the cursor spans. A change that moved only
//! the picture, or only the state, disagrees with one of the two.

#![cfg(feature = "app")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

/// Twenty seconds at 120 bpm: a beat every half-second, so a snap lands on a
/// number the assertions can state exactly.
const TRACK_SECONDS: u32 = 20;

/// Three clips over two lanes.
///
/// `Wash` and `Strobe` are deliberately the same span in different lanes, so a
/// rectangle drawn over both of them selects two clips and a rectangle drawn
/// over one selects one — which is the whole difference the marquee's row band
/// is supposed to make. `Haze` sits early and alone, as the clip that must
/// *not* be caught by a sweep that starts after it.
const CLIPS: [(&str, &str, f64, f64, i64); 3] = [
    ("pattern-haze", "Haze", 2.0, 6.0, 0),
    ("pattern-strobe", "Strobe", 14.0, 18.0, 0),
    ("pattern-wash", "Wash", 14.0, 18.0, 1),
];

/// The opening zoom, in pixels per second. Every distance below is stated in
/// pixels and read back in seconds through it.
const ZOOM: f64 = 50.;

fn harness() -> Harness {
    Fixture::new(
        "track-editor-ux",
        TRACK_SECONDS,
        CLIPS
            .iter()
            .map(|(pattern, name, start, end, z)| Clip::new(*pattern, *name, *start, *end).lane(*z))
            .collect(),
    )
    .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    function shot() { return app.snapshot(); }

    function status() {
        return shot().findAll({ role: "text" }).map((n) => n.label);
    }

    /** One toolbar readout by prefix, or null. */
    function readout(prefix) {
        return status().find((label) => label.startsWith(prefix)) ?? null;
    }

    function node(role, label) {
        return shot().find({ role, label });
    }

    /** Every clip's drawn extent, in seconds, off its header node. */
    function clips() {
        const out = {};
        for (const card of shot().findAll({ role: "card" })) {
            if (card.label === "Waveform" || card.label === "Ruler") continue;
            out[card.label] = {
                start: card.bounds.x / ZOOM,
                length: card.bounds.width / ZOOM,
                y: card.bounds.y,
                height: card.bounds.height,
            };
        }
        return out;
    }

    /** Wait until every queued write has landed, or say it never did. */
    function settled() {
        for (let i = 0; i < 60; i++) {
            if (!status().includes("SAVING")) return true;
            app.frames(1, { waitMs: 40 });
        }
        throw new Error("a write never left the editor");
    }

    function playhead() {
        const head = node("slider", "Playhead");
        return head === undefined ? null : head.bounds.x;
    }

    /** Open the track from the browser. Back lands there, so this is the way
        in both the first time and after a reopen. */
    function open() {
        app.click(node("row", "Aurora"));
        // The editor's opening load is five commands on a runtime gpui does
        // not own, one of which decodes and renders the audio.
        app.frames(20);
    }

    app.click(node("card", "Test Venue"));
    app.frames(8);
    open();
    const opened = { clips: clips(), status: status(), playhead: playhead() };

    // --- press regions -----------------------------------------------------
    // The ruler scrubs. The waveform under it does not: it clears the
    // selection, which is the one surprise in the web's pointer map.

    app.click(node("card", "Strobe"));
    app.frames(2);
    const clipPressed = { status: status(), cursor: node("slider", "Cursor") !== undefined };

    app.click(node("card", "Waveform"));
    app.frames(2);
    const waveformPressed = {
        status: status(),
        playhead: playhead(),
        cursor: node("slider", "Cursor") !== undefined,
    };

    const ruler = node("card", "Ruler");
    app.click(ruler);
    app.frames(4);
    const rulerPressed = {
        playhead: playhead(),
        want: ruler.bounds.x + ruler.bounds.width / 2,
    };

    // The empty insertion lane above the top layer is the other clearing
    // surface.
    app.click(node("card", "Strobe"));
    app.frames(2);
    app.click(node("row", "Lane 0"));
    app.frames(2);
    const laneZeroPressed = status();

    // --- clip header is the only grab --------------------------------------
    // A clip's node is its header bar, 18px tall, not its 80px lane.

    const geometry = clips();

    // --- marquee -----------------------------------------------------------
    // Sweep right from the middle of the bottom lane. Strobe (14-18s) is to
    // the right of the middle; Haze (2-6s) is not, and Wash is in the lane
    // above.

    /** Sweep right from the middle of the bottom lane, `dy` lanes' worth up. */
    function sweep(dy) {
        const lane = node("row", "Lane 2");
        app.drag(lane, { dx: lane.bounds.width / 2 - 2, dy });
        app.frames(2);
        return { status: status(), cursor: readout("CURSOR ") };
    }

    const sweptOneLane = sweep(0);

    // The same sweep, one lane up: two rows, and both clips in the band.
    const sweptTwoLanes = sweep(-80);

    // --- multi-clip resize -------------------------------------------------
    // With Strobe and Wash both selected, pulling Strobe's end pulls Wash's.

    const beforeResize = clips();
    app.drag(node("slider", "Strobe end"), { dx: 100, dy: 0 });
    app.frames(20);
    const resized = clips();
    settled();

    // --- resize snapping ---------------------------------------------------
    // At this zoom the grid is a quarter of a half-second beat, so a 90px drag
    // — 1.8 seconds — is captured to 1.75. Select one clip first, so this is a
    // single-clip resize and the number is not a group's.

    app.click(node("card", "Haze"));
    app.frames(2);
    const beforeSnap = clips();
    app.drag(node("slider", "Haze end"), { dx: 90, dy: 0 });
    app.frames(20);
    const snapped = clips();
    settled();

    // --- move --------------------------------------------------------------
    // Dragging a clip's header sideways moves the whole clip, and it stays
    // moved across a reopen.

    const beforeMove = clips();
    app.drag(node("card", "Haze"), { dx: 100, dy: 0 });
    app.frames(20);
    const moved = clips();

    // Leaving before the write lands would read the score back unchanged for
    // a reason that is not the one under test.
    settled();
    app.click(node("button", "Back"));
    app.frames(6);
    open();
    const reopened = clips();

    // --- wheel -------------------------------------------------------------
    // Twenty seconds at the opening zoom is narrower than the window, so there
    // is nothing to scroll until the zoom makes the content wider than the
    // viewport. Zoom first, then scroll against it.
    //
    // Everything is read off `Haze`, which is the clip near the start of the
    // track: a node's bounds are clipped to what is on screen, so a clip that
    // the zoom pushed off the right edge would read back as zero wide.

    /** The wheel's target, re-found each time: every scroll redraws it. */
    function waveform() {
        return node("card", "Waveform");
    }

    /** Scroll hard left, which lands at zero however far away it started. */
    function toStart() {
        app.scroll(waveform(), { dx: 80000, steps: 20 });
        app.frames(2);
    }

    app.scroll(waveform(), { dy: 300, steps: 10, modifiers: ["platform"] });
    toStart();
    const zoomedIn = shot().find({ role: "card", label: "Haze" }).bounds.width;
    const atRest = clips();

    app.scroll(waveform(), { dx: -200, steps: 10 });
    app.frames(2);
    const scrolled = clips();

    // Far past the end, twice — a scroll that clamps lands in the same place
    // both times.
    app.scroll(waveform(), { dx: -40000, steps: 20 });
    app.frames(2);
    const scrolledToEnd = clips();
    app.scroll(waveform(), { dx: -40000, steps: 20 });
    app.frames(2);
    const scrolledFurther = clips();

    toStart();
    const scrolledBack = clips();

    // And all the way out, where the zoom clamps at MIN_ZOOM.
    app.scroll(waveform(), { dy: -20000, steps: 20, modifiers: ["platform"] });
    toStart();
    const zoomedOut = shot().find({ role: "card", label: "Haze" }).bounds.width;

    // --- follow playhead ---------------------------------------------------

    app.key("f");
    app.frames(2);
    const following = status();
    app.key("f");
    app.frames(2);
    const unfollowed = status();

    ({
        opened,
        clipPressed,
        waveformPressed,
        rulerPressed,
        laneZeroPressed,
        geometry,
        sweptOneLane,
        sweptTwoLanes,
        beforeResize,
        resized,
        beforeSnap,
        snapped,
        beforeMove,
        moved,
        reopened,
        atRest,
        scrolled,
        scrolledToEnd,
        scrolledFurther,
        scrolledBack,
        zoomedIn,
        zoomedOut,
        following,
        unfollowed,
    })
"#;

#[test]
fn the_timeline_answers_the_pointer_and_the_wheel_the_way_the_web_one_does() {
    let mut harness = harness();
    let script = SCRIPT.replace("ZOOM", &ZOOM.to_string());
    let result = harness.exec(&script, Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // 0. The screen loaded and named everything.
    let opened = &out["opened"];
    assert!(
        labels(opened, "status").contains(&"3 CLIPS".to_string()),
        "the editor did not open on the seeded score: {opened:#}"
    );

    // 1. Press regions. A clip selects and sets a cursor; the waveform clears
    //    both and does *not* move the playhead; the ruler seeks.
    assert!(
        labels(&out["clipPressed"], "status").contains(&"1 SELECTED".to_string()),
        "pressing a clip's header did not select it: {:#}",
        out["clipPressed"]
    );
    assert_eq!(
        out["clipPressed"]["cursor"], true,
        "pressing a clip did not set the selection cursor"
    );
    let waveform = &out["waveformPressed"];
    assert!(
        !labels(waveform, "status")
            .iter()
            .any(|label| label.ends_with("SELECTED")),
        "pressing the waveform left the selection alone; it should clear it: {waveform:#}"
    );
    assert_eq!(
        waveform["cursor"], false,
        "pressing the waveform left the cursor up: {waveform:#}"
    );
    assert_eq!(
        waveform["playhead"], opened["playhead"],
        "pressing the waveform moved the playhead; only the ruler scrubs: {waveform:#}"
    );
    let ruler = &out["rulerPressed"];
    let (landed, wanted) = (number(ruler, "playhead"), number(ruler, "want"));
    assert!(
        (landed - wanted).abs() <= 1.,
        "a press on the ruler seeked to {landed}, not to the {wanted} under it"
    );

    // 2. The empty insertion lane above the top layer clears the selection.
    assert!(
        !labels(&out["laneZeroPressed"], "")
            .iter()
            .any(|label| label.ends_with("SELECTED")),
        "pressing the empty row-0 lane did not clear the selection: {:#}",
        out["laneZeroPressed"]
    );

    // 3. Only a clip's 18px header answers the pointer, so that is the extent
    //    a script can act on — the 80px lane below it is inert.
    for (_, label, ..) in CLIPS {
        assert_eq!(
            out["geometry"][label]["height"].as_f64(),
            Some(18.),
            "{label}'s grabbable extent is not its header bar: {:#}",
            out["geometry"]
        );
    }

    // 4. A sweep selects what its rectangle *contains*. One lane catches one
    //    clip; the same sweep a lane taller catches two; neither catches the
    //    clip that starts before the sweep did.
    let one = &out["sweptOneLane"];
    assert!(
        labels(one, "status").contains(&"1 SELECTED".to_string()),
        "a one-lane sweep should have caught Strobe alone: {one:#}"
    );
    let two = &out["sweptTwoLanes"];
    assert!(
        labels(two, "status").contains(&"2 SELECTED".to_string()),
        "a two-lane sweep should have caught Strobe and Wash: {two:#}"
    );
    assert!(
        two["cursor"]
            .as_str()
            .is_some_and(|label| label.contains('-')),
        "a sweep should leave a range cursor, not a point: {two:#}"
    );

    // 5. A resize moves the same edge of every selected clip by the same
    //    delta, and leaves their starts alone.
    let (before, after) = (&out["beforeResize"], &out["resized"]);
    for label in ["Strobe", "Wash"] {
        assert!(
            (span(after, label).1 - span(before, label).1 - 2.).abs() < 0.01,
            "{label}'s end did not follow the group resize: {before:#} -> {after:#}"
        );
        assert!(
            (span(after, label).0 - span(before, label).0).abs() < 0.01,
            "{label}'s start drifted during an end resize"
        );
    }

    // 6. A resize snaps to the beat grid. The grid at this zoom is a quarter
    //    of a half-second beat, so 1.8 seconds of drag is captured to 1.75 —
    //    a number the drag distance alone could not produce.
    let (before, after) = (&out["beforeSnap"], &out["snapped"]);
    let moved = span(after, "Haze").1 - span(before, "Haze").1;
    assert!(
        (moved - 1.75).abs() < 0.01,
        "the resize moved the edge by {moved}s; snapped to the grid it is 1.75s"
    );

    // 7. A move takes the whole clip and survives a reopen — it is a write,
    //    not a repaint.
    let (before, after) = (&out["beforeMove"], &out["moved"]);
    let (was, now) = (span(before, "Haze"), span(after, "Haze"));
    assert!(
        (now.0 - was.0 - 2.0).abs() < 0.01 && (now.1 - was.1 - 2.0).abs() < 0.01,
        "a header drag should slide the whole clip by 2.0s: {was:?} -> {now:?}"
    );
    let reopened = span(&out["reopened"], "Haze");
    assert!(
        (reopened.0 - now.0).abs() < 0.01 && (reopened.1 - now.1).abs() < 0.01,
        "the move did not survive a reopen: {now:?} -> {reopened:?}"
    );

    // 8. A bare wheel scrolls, and stops at the end of the content rather
    //    than running off into empty space.
    let (rest, scrolled) = (&out["atRest"], &out["scrolled"]);
    let travelled = (span(rest, "Haze").0 - span(scrolled, "Haze").0) * ZOOM;
    assert!(
        (travelled - 200.).abs() < 1.,
        "a 200px wheel scrolled the timeline {travelled}px: {rest:#} -> {scrolled:#}"
    );
    // Read at the far end off a clip that is still on screen there — a node's
    // bounds are clipped to the viewport, and two clips both scrolled out of
    // it would agree for the wrong reason.
    let end = span(&out["scrolledToEnd"], "Wash").0;
    let further = span(&out["scrolledFurther"], "Wash").0;
    assert!(
        end > 1. && (end - further).abs() < 0.001,
        "scrolling past the end kept going: {end}s then {further}s"
    );

    // 9. A modified wheel zooms, both ways, and clamps at MIN_ZOOM. Read back
    //    off a clip of known length, which is the only thing on screen whose
    //    pixels state the zoom.
    let length = {
        let (from, to) = span(&out["reopened"], "Haze");
        to - from
    };
    let zoomed_in = out["zoomedIn"].as_f64().unwrap_or(0.) / length;
    assert!(
        zoomed_in > 1.5 * ZOOM,
        "a platform-wheel zoom in only reached {zoomed_in} px/s"
    );

    // Scrolling back the other way lands at zero, which puts Wash's start
    // where its own start time says. Stated through the zoom, because the
    // readings are pixels over the *opening* zoom and the view is not there
    // any more.
    let back = span(&out["scrolledBack"], "Haze").0 * ZOOM / zoomed_in;
    assert!(
        (back - span(&out["reopened"], "Haze").0).abs() < 0.05,
        "scrolling back should reach the start of the track, not {back}s"
    );

    let zoomed_out = out["zoomedOut"].as_f64().unwrap_or(0.) / length;
    assert!(
        (zoomed_out - 25.).abs() < 0.5,
        "zooming out should stop at MIN_ZOOM, not {zoomed_out} px/s"
    );

    // 10. `F` toggles following the playhead, and says so.
    assert!(
        labels(&out, "following").contains(&"FOLLOW".to_string()),
        "F did not turn on follow-playhead: {:#}",
        out["following"]
    );
    assert!(
        !labels(&out, "unfollowed").contains(&"FOLLOW".to_string()),
        "F did not turn follow-playhead back off: {:#}",
        out["unfollowed"]
    );
}

/// One clip's drawn span, in seconds.
fn span(reading: &Value, label: &str) -> (f64, f64) {
    let clip = &reading[label];
    let start = clip["start"]
        .as_f64()
        .unwrap_or_else(|| panic!("{label} has no start: {reading:#}"));
    let length = clip["length"]
        .as_f64()
        .unwrap_or_else(|| panic!("{label} has no length: {reading:#}"));
    (start, start + length)
}

/// A list of strings out of a reading, by key — or the reading itself when the
/// key is empty, for the sections that are a bare list.
fn labels(reading: &Value, key: &str) -> Vec<String> {
    let value = if key.is_empty() {
        reading
    } else {
        &reading[key]
    };
    serde_json::from_value(value.clone()).unwrap_or_default()
}

fn number(reading: &Value, key: &str) -> f64 {
    reading[key]
        .as_f64()
        .unwrap_or_else(|| panic!("no {key} in {reading:#}"))
}
