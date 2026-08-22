//! The track editor's pointer and view contract, driven through real input.
//!
//! Everything here is a behavior the web timeline has and this canvas had to
//! grow: which vertical band answers a press, that only a clip's header bar is
//! grabbable, that a sweep of empty lane selects what it *contains*, that a
//! resize snaps to the beat grid and moves every selected clip, that every
//! destructive command steps back under Cmd+Z, that two clips may share a
//! layer, and that the wheel scrolls where it is not zooming. The contract
//! they are checked against is `harness/gauntlet-te/behavior-spec.md`, which
//! is the web source turned into an index.
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
    // Every pixel-arithmetic premise in SCRIPT was authored against a
    // 1200-wide canvas. In takeover the shell spends 280 of the row (the
    // sidebar and the card gaps) and 46 of the column (the titlebar band and
    // the bottom gap), so the window grows by exactly that much and the
    // timeline keeps its size.
    .window(1480., 818.)
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

    /** Where time zero is on screen: the canvas origin, off the waveform
     *  strip. Subtracted from every x so a reading is track time, not window
     *  geometry — the shell's sidebar sits left of the canvas. */
    function origin() {
        return node("card", "Waveform").bounds.x;
    }

    /** Every clip's drawn extent, in seconds, off its header node. */
    function clips() {
        const zero = origin();
        const out = {};
        for (const card of shot().findAll({ role: "card" })) {
            if (card.label === "Waveform" || card.label === "Ruler") continue;
            out[card.label] = {
                start: (card.bounds.x - zero) / ZOOM,
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
        nav.track("Aurora");
        // The editor's opening load is five commands on a runtime gpui does
        // not own, one of which decodes and renders the audio.
        // Waited for by its result (the timeline's waveform card), not by a
        // frame count: nav.track returns as soon as the row was pressable,
        // which is earlier than the old hand-rolled walk got here, so a bare
        // frame count can land inside the load.
        until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
        nav.expand();
    }

    nav.venue("Test Venue");
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
    nav.closeTab();
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

    // --- loop region -------------------------------------------------------
    // Cmd+L loops the cursor's range, and a second press over the same range
    // takes the loop off. The chord used to open the agent chat; the chat
    // answers to cmd-shift-l now, so nothing shadows the editor's own key.

    const loopLane = shot().find({ role: "row", label: "Lane 2" });
    app.drag(loopLane, { dx: 200, dy: 0 });
    app.frames(2);
    const loopCursor = readout("CURSOR ");
    app.key("cmd-l");
    app.frames(2);
    const looped = { region: readout("LOOP "), status: status() };
    app.key("cmd-l");
    app.frames(2);
    const unlooped = readout("LOOP ");

    // --- editing commands --------------------------------------------------
    // Everything below is a *write*: the working copy changes and one
    // compare-and-swap publishes it. Each section therefore reads back twice —
    // once from the picture, once after a reopen, which is the only proof the
    // score and not just the canvas moved.
    //
    // Reopening first also puts the view back at the opening zoom with no
    // scroll, so the pixel arithmetic above still holds.

    /** How many clips carry this label. Duplicates are the point of half the
        sections below, so nothing here may key a map by label. */
    function count(label) {
        return shot().findAll({ role: "card" }).filter((c) => c.label === label).length;
    }
    function total() {
        return shot().findAll({ role: "card" })
            .filter((c) => c.label !== "Waveform" && c.label !== "Ruler").length;
    }
    function reopen() {
        settled();
        nav.closeTab();
        app.frames(6);
        open();
    }

    reopen();
    /** Every card with this label, left to right. */
    function spans(label) {
        const zero = origin();
        return shot().findAll({ role: "card" })
            .filter((c) => c.label === label)
            .map((c) => ({
                start: (c.bounds.x - zero) / ZOOM,
                length: c.bounds.width / ZOOM,
                y: c.bounds.y,
            }))
            .sort((a, b) => a.start - b.start);
    }
    function laneBox(label) {
        const lane = shot().find({ role: "row", label });
        return {
            top: lane.bounds.y,
            bottom: lane.bounds.y + lane.bounds.height,
            width: lane.bounds.width,
        };
    }
    const editable = { clips: clips(), total: total(), lane: laneBox("Lane 2") };

    // Duplicate: Cmd+D copies the cursor's region and lays it down immediately
    // after itself, clearing whatever the destination already held.
    app.click(node("card", "Haze"));
    app.frames(2);
    app.key("cmd-d");
    app.frames(20);
    settled();
    const duplicated = { hazes: count("Haze"), total: total() };

    // Delete: the copy is still selected and the cursor still spans it, so
    // Delete clears exactly that region and leaves the original alone.
    app.key("delete");
    app.frames(20);
    settled();
    reopen();
    const deleted = { hazes: count("Haze"), total: total() };

    // Undo: every mutating command is reversible, and the reversal is itself
    // a write — so the proof is not only that the clip comes back on screen
    // but that it is there again after a reopen.
    //
    // No reopen inside the sequence: the history belongs to the screen, and
    // leaving the editor is leaving it behind. `ctrl-z` at the end is the
    // other spelling of the same chord, which the web reads too.
    const beforeUndo = { total: total(), strobes: count("Strobe") };
    app.click(node("card", "Strobe"));
    app.frames(2);
    app.key("delete");
    app.frames(20);
    const afterDelete = { total: total(), strobes: count("Strobe") };
    app.key("cmd-z");
    app.frames(20);
    const undone = { total: total(), strobes: count("Strobe") };
    app.key("cmd-shift-z");
    app.frames(20);
    const redone = { total: total(), strobes: count("Strobe") };
    app.key("ctrl-z");
    app.frames(20);
    settled();
    reopen();
    const undoneStored = { total: total(), strobes: count("Strobe") };

    // Split: put Haze under the middle of the lane, set the cursor there with
    // an empty-lane press — the lane's centre is below the 18px header band,
    // so that press lands on the clip's inert body — and cut it in two.
    const middle = laneBox("Lane 2").width / 2 / ZOOM;
    const haze = clips()["Haze"];
    app.drag(node("card", "Haze"), {
        dx: (middle - (haze.start + haze.length / 2)) * ZOOM,
        dy: 0,
    });
    app.frames(20);
    settled();
    const straddling = spans("Haze")[0];
    app.click(shot().find({ role: "row", label: "Lane 2" }));
    app.frames(2);
    app.key("cmd-e");
    app.frames(20);
    settled();
    reopen();
    const splitHalves = spans("Haze");

    // Vertical drag: pull the left half up a lane. Its z becomes the layer
    // above — which is where Wash lives — so it ends up sharing Wash's lane,
    // and stays there across a reopen, which a paint-only row offset could
    // not do.
    const beforeLift = { hazes: spans("Haze"), strobe: spans("Strobe")[0] };
    app.drag(node("card", "Haze"), { dx: 0, dy: -80 });
    app.frames(20);
    settled();
    const lifted = { hazes: spans("Haze"), wash: spans("Wash")[0] };
    reopen();
    const relifted = { hazes: spans("Haze"), strobe: spans("Strobe")[0] };

    // Alt-drag: the copy stays where the press was and the original is what
    // the pointer takes away.
    const beforeAlt = spans("Strobe")[0];
    app.drag(node("card", "Strobe"), { dx: -500, dy: 0 }, { modifiers: ["alt"] });
    app.frames(20);
    settled();
    reopen();
    const strobes = spans("Strobe");

    // Overlap: two clips may share a layer *and* a span. The web timeline
    // says so — it specifies which of two overlapping clips a press picks —
    // so a move across a neighbour has to survive the write rather than be
    // painted, accepted and then quietly rolled back on the next visit.
    /** The leftmost card carrying this label. */
    function leftmost(label) {
        return shot().findAll({ role: "card" })
            .filter((c) => c.label === label)
            .sort((a, b) => a.bounds.x - b.bounds.x)[0];
    }
    const beforeOverlap = spans("Strobe");
    app.drag(leftmost("Strobe"), { dx: 350, dy: 0 });
    app.frames(20);
    const overlapped = { strobes: spans("Strobe"), status: status() };
    settled();
    reopen();
    const overlapStored = { strobes: spans("Strobe"), status: status() };

    // Right-click: the insertion menu, and the clip it commits. Row 0 opens a
    // lane above everything, so the inserted clip cannot overlap what is
    // already there whichever pattern is chosen.
    const beforeInsert = total();
    // The menu's rows are whichever rows the right-click *added*: the lane
    // headers and the sidebar's track rows are rows too, so "every row that
    // is not a lane" would pick up the track list.
    const rowsBefore = shot().findAll({ role: "row" }).map((n) => n.label);
    app.click(shot().find({ role: "row", label: "Lane 0" }), { button: "right" });
    app.frames(2);
    const menu = shot()
        .findAll({ role: "row" })
        .map((n) => n.label)
        .filter((label) => !rowsBefore.includes(label));
    app.click(shot().find({ role: "row", label: "Wash" }));
    app.frames(20);
    settled();
    reopen();
    const inserted = { total: total(), washes: spans("Wash") };

    // The menu also has a keyboard: ArrowDown moves the active row and Enter
    // commits *that* one, so what lands is the second pattern and not the
    // first. A menu a key could open and only a pointer could answer is a menu
    // that wedges the screen.
    const beforeKeyed = { total: total(), chosen: count(menu[1]) };
    app.click(shot().find({ role: "row", label: "Lane 0" }), { button: "right" });
    app.frames(2);
    app.key("down");
    app.frames(2);
    app.key("enter");
    app.frames(20);
    settled();
    reopen();
    const keyed = { total: total(), chosen: count(menu[1]) };

    // And Escape puts it away without leaving the screen, which is the other
    // half of not being wedged.
    app.click(shot().find({ role: "row", label: "Lane 0" }), { button: "right" });
    app.frames(2);
    const menuOpen = shot().findAll({ role: "row" }).some((n) => n.label === menu[0]);
    app.key("escape");
    app.frames(2);
    const dismissed = {
        open: shot().findAll({ role: "row" }).some((n) => n.label === menu[0]),
        editor: node("card", "Ruler") !== undefined,
        total: total(),
    };

    // Follow re-centres while the transport is *stopped*: the playhead moves
    // because the pointer moved it, and the eye still has to keep up.
    //
    // The keystroke comes *before* the wheel deliberately. A prior round left
    // open whether a wheel that follows a keystroke still reaches the canvas;
    // the zoom below is exactly that, and the re-centring assertion cannot
    // pass unless it landed — a timeline narrower than its viewport has
    // nowhere to scroll to.
    app.key("f");
    app.frames(2);
    app.scroll(waveform(), { dy: 600, steps: 10, modifiers: ["platform"] });
    toStart();
    const strip = node("card", "Ruler");
    // One step, so the press and the move are the whole gesture: the press
    // lands under the pointer and the move is 300px to the right of it, which
    // a following eye pulls back to the middle and a still one leaves where it
    // fell.
    app.drag(strip, { dx: 300, dy: 0 }, { steps: 1 });
    app.frames(6);
    const followed = {
        playhead: playhead(),
        centre: strip.bounds.x + strip.bounds.width / 2,
    };

    // Double-click opens the clip's pattern. Last, because it navigates away —
    // and from a reopened view, because the section above left the timeline
    // zoomed in with the clips off the right-hand edge.
    reopen();
    app.click(node("card", "Wash"), { count: 2 });
    app.frames(20);
    const navigated = shot().find({ role: "card", label: "Ruler" }) === undefined;

    // ... and Back returns to the timeline it came from, not the patterns
    // list: the graph screen carries the screen it was opened over.
    app.action("luma::CloseTab");
    app.frames(12);
    const returned = shot().find({ role: "card", label: "Ruler" }) !== undefined;

    ({
        editable,
        duplicated,
        deleted,
        beforeUndo,
        afterDelete,
        undone,
        redone,
        undoneStored,
        beforeOverlap,
        overlapped,
        overlapStored,
        loopCursor,
        looped,
        unlooped,
        straddling,
        splitHalves,
        beforeLift,
        lifted,
        relifted,
        beforeAlt,
        strobes,
        beforeInsert,
        menu,
        inserted,
        beforeKeyed,
        keyed,
        menuOpen,
        dismissed,
        followed,
        navigated,
        returned,
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
    let result = harness.exec(&support::script(&script), Duration::from_secs(300));
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
    // One device pixel of tolerance, not half of one: layout snaps boxes to
    // whole pixels, and a snapped end that lands on a half-pixel boundary
    // (1.75s at 50px/s is 87.5px) reads back one pixel short. The raw drag is
    // 2.5px from the grid, so a 1px tolerance still tells snap from no-snap.
    assert!(
        (moved - 1.75).abs() <= 0.02,
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

    // 11. Duplicate lays the cursor's region down again immediately after
    //     itself, in the same lane — and it is a real clip, not a paint.
    let (before, after) = (&out["editable"], &out["duplicated"]);
    assert_eq!(
        after["hazes"].as_u64(),
        Some(2),
        "Cmd+D did not duplicate the selected clip: {before:#} -> {after:#}"
    );
    assert_eq!(
        after["total"].as_u64(),
        before["total"].as_u64().map(|total| total + 1),
        "Cmd+D changed the score by more than the one clip it copied"
    );

    // 12. Delete clears the cursor's region and nothing else, and the score
    //     comes back that way.
    let deleted = &out["deleted"];
    assert_eq!(
        deleted["hazes"].as_u64(),
        Some(1),
        "Delete did not take the copy back out: {deleted:#}"
    );
    assert_eq!(
        deleted["total"].as_u64(),
        before["total"].as_u64(),
        "after a duplicate and a delete the score should be back where it started: {deleted:#}"
    );

    // 12a. Undo puts a destructive command back, redo takes it away again,
    //      and both are writes — the reopen is what proves the score moved
    //      and not only the canvas.
    let (was, gone) = (&out["beforeUndo"], &out["afterDelete"]);
    assert_eq!(
        gone["strobes"].as_u64(),
        Some(0),
        "the delete under test did not remove the clip: {was:#} -> {gone:#}"
    );
    let undone = &out["undone"];
    assert_eq!(
        (undone["strobes"].as_u64(), undone["total"].as_u64()),
        (was["strobes"].as_u64(), was["total"].as_u64()),
        "Cmd+Z did not put the deleted clip back: {gone:#} -> {undone:#}"
    );
    assert_eq!(
        out["redone"]["strobes"].as_u64(),
        Some(0),
        "Cmd+Shift+Z did not re-apply the delete: {:#}",
        out["redone"]
    );
    let stored = &out["undoneStored"];
    assert_eq!(
        (stored["strobes"].as_u64(), stored["total"].as_u64()),
        (was["strobes"].as_u64(), was["total"].as_u64()),
        "an undo is a write: the score should come back undone, not deleted: {stored:#}"
    );

    // 12b. A clip may be moved across its neighbour on the same layer, and
    //      the move survives the write. The failure this pins is the one that
    //      painted the edit, accepted it, and rolled it back on the next
    //      visit with an internal error for a warning.
    let laid_out = out["beforeOverlap"].as_array().cloned().unwrap_or_default();
    let crossed = out["overlapped"]["strobes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        (laid_out.len(), crossed.len()),
        (2, 2),
        "the overlap section needs the two clips the alt-drag left: {laid_out:#?}"
    );
    let moved = number(&crossed[0], "start");
    assert!(
        (moved - (number(&laid_out[0], "start") + 7.)).abs() < 0.01,
        "the drag should have slid the clip 7s right, to overlap its neighbour: {crossed:#?}"
    );
    assert!(
        moved + number(&crossed[0], "length") > number(&crossed[1], "start"),
        "the two clips do not overlap, so this proves nothing: {crossed:#?}"
    );
    let restored = out["overlapStored"]["strobes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        restored.len(),
        2,
        "the overlapping move lost a clip: {restored:#?}"
    );
    assert!(
        (number(&restored[0], "start") - moved).abs() < 0.01,
        "the overlapping move did not survive a reopen: {moved}s -> {restored:#?}"
    );
    for reading in ["overlapped", "overlapStored"] {
        assert!(
            !labels(&out[reading], "status")
                .iter()
                .any(|label| label.contains("overlap")),
            "the editor reported an overlap refusal: {:#}",
            out[reading]
        );
    }

    // 12c. Cmd+L takes the loop from the cursor's range, and takes it off
    //      again when asked for the same one — and it reaches the editor
    //      rather than the agent chat that used to hold the chord.
    let looped = &out["looped"];
    let region = looped["region"].as_str().unwrap_or_default();
    let cursor = out["loopCursor"].as_str().unwrap_or_default();
    assert!(
        !region.is_empty()
            && region.trim_start_matches("LOOP ") == cursor.trim_start_matches("CURSOR "),
        "Cmd+L should loop exactly the cursor's range: {cursor:?} -> {looped:#}"
    );
    assert!(
        !labels(looped, "status")
            .iter()
            .any(|label| label == "Track agent"),
        "Cmd+L opened the agent chat instead of setting the loop: {looped:#}"
    );
    assert_eq!(
        out["unlooped"],
        Value::Null,
        "a second Cmd+L over the same range should clear the loop"
    );

    // 13. The lane block is bottom-anchored: the last lane's floor is the
    //     canvas's, not 192 + N*80 down from the top.
    let lane = &before["lane"];
    assert!(
        number(lane, "bottom") > 600.,
        "the lanes are not pinned to the bottom of the canvas: {lane:#}"
    );

    // 14. Split cuts the clip the cursor crosses into two halves that together
    //     cover exactly what the one clip did, and the score comes back split.
    let straddling = &out["straddling"];
    let halves = out["splitHalves"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        halves.len(),
        2,
        "Cmd+E did not split the clip under the cursor: {straddling:#} -> {halves:#?}"
    );
    let (was_start, was_length) = (number(straddling, "start"), number(straddling, "length"));
    let covered: f64 = halves.iter().map(|half| number(half, "length")).sum();
    assert!(
        (number(&halves[0], "start") - was_start).abs() < 0.01
            && (covered - was_length).abs() < 0.05,
        "the halves do not cover the clip they came from: {straddling:#} -> {halves:#?}"
    );

    // 15. A vertical drag is a z-index write, not a paint offset: the half
    //     that was pulled up lands in the lane above and is still there after
    //     a reopen. Read as a *gap* between the two halves, which were in one
    //     lane before and cannot be in one after.
    let (was, now, again) = (&out["beforeLift"], &out["lifted"], &out["relifted"]);
    let lane_gap = |reading: &Value| {
        let hazes = reading["hazes"].as_array().cloned().unwrap_or_default();
        assert_eq!(hazes.len(), 2, "the split halves went missing: {reading:#}");
        number(&hazes[1], "y") - number(&hazes[0], "y")
    };
    assert!(
        lane_gap(was).abs() < 1.,
        "the two halves should start out in one lane: {was:#}"
    );
    assert!(
        (lane_gap(now) - 80.).abs() < 1.,
        "an upward drag should have lifted one half a lane: {now:#}"
    );
    assert!(
        (lane_gap(again) - 80.).abs() < 1.,
        "the lane change did not survive a reopen: {again:#}"
    );
    assert!(
        (number(&now["hazes"][0], "y") - number(&now["wash"], "y")).abs() < 1.,
        "the lifted half should share the layer above, which is Wash's: {now:#}"
    );
    // Bottom-anchored: the lane count did not change, so nothing else moved.
    assert!(
        (number(&was["strobe"], "y") - number(&again["strobe"], "y")).abs() < 1.,
        "the layer on the floor moved when another clip changed lane"
    );

    // 16. Alt+drag duplicates in place: the copy is left where the press was
    //     and the original is what the pointer took away.
    let strobes = out["strobes"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        strobes.len(),
        2,
        "an alt-drag should have left a copy behind: {strobes:#?}"
    );
    let from = number(&out["beforeAlt"], "start");
    let (moved, stayed) = (number(&strobes[0], "start"), number(&strobes[1], "start"));
    assert!(
        (stayed - from).abs() < 0.01 && (moved - (from - 10.)).abs() < 0.01,
        "the copy should stay at {from}s and the original move 10s back: {stayed}s and {moved}s"
    );

    // 17. A right-click offers the library's patterns and commits one onto the
    //     lane it pointed at — row 0, which opens a layer above everything.
    let menu = labels(&out, "menu");
    for (_, name, ..) in CLIPS {
        assert!(
            menu.contains(&name.to_string()),
            "the insertion menu did not offer {name}: {menu:?}"
        );
    }
    let inserted = &out["inserted"];
    assert_eq!(
        inserted["total"].as_u64(),
        out["beforeInsert"].as_u64().map(|total| total + 1),
        "the insertion menu did not add a clip: {inserted:#}"
    );
    let washes = inserted["washes"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        washes.len(),
        2,
        "the clip the menu inserted is not the pattern that was chosen: {inserted:#}"
    );
    let lanes: Vec<f64> = washes.iter().map(|clip| number(clip, "y")).collect();
    assert!(
        (lanes[0] - lanes[1]).abs() > 79.,
        "an insertion on row 0 should open a lane of its own above the rest: {inserted:#}"
    );

    // 17a. The menu answers the keyboard: Enter commits the row the arrows
    //      left active, and Escape closes it without leaving the editor.
    let (was, keyed) = (&out["beforeKeyed"], &out["keyed"]);
    assert_eq!(
        keyed["total"].as_u64(),
        was["total"].as_u64().map(|total| total + 1),
        "Enter did not commit the insertion menu: {was:#} -> {keyed:#}"
    );
    assert_eq!(
        keyed["chosen"].as_u64(),
        was["chosen"].as_u64().map(|of| of + 1),
        "Enter committed a pattern the arrow key had moved off: {keyed:#}"
    );
    assert_eq!(
        out["menuOpen"], true,
        "the right-click did not open the menu, so Escape proves nothing"
    );
    let dismissed = &out["dismissed"];
    assert_eq!(
        (dismissed["open"].as_bool(), dismissed["editor"].as_bool()),
        (Some(false), Some(true)),
        "Escape should close the menu and leave the timeline up: {dismissed:#}"
    );
    assert_eq!(
        dismissed["total"].as_u64(),
        keyed["total"].as_u64(),
        "closing the menu inserted a clip: {dismissed:#}"
    );

    // 17. With follow on, a scrub that moves the playhead pulls the view back
    //     under it — the transport is *stopped*, so nothing else would.
    let followed = &out["followed"];
    let (landed, centre) = (number(followed, "playhead"), number(followed, "centre"));
    assert!(
        (landed - centre).abs() <= 2.,
        "a followed scrub left the playhead at {landed}, not re-centred at {centre}: {followed:#}"
    );

    // 18. Double-clicking a clip leaves the timeline for its pattern — and
    //     Back restores that timeline whole, not the patterns list.
    assert_eq!(
        out["navigated"], true,
        "a double-click on a clip did not open its pattern"
    );
    assert_eq!(
        out["returned"], true,
        "Back from a double-click-opened pattern did not return to the timeline"
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
