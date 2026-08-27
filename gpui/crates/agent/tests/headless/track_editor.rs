//! The track editor, driven end to end against a seeded library.
//!
//! The point of this test is that *a clip's bounds are a document and the
//! playhead is not*. Dragging a clip's edge is a write — it goes through
//! `update_track_score` and comes back as the authoritative clip list — so the
//! test moves an edge, reads it, then leaves the screen and comes back,
//! because only the second reading can tell a repaint from a write. Playback
//! is the opposite: nothing is written, and the only honest evidence is that
//! the transport moved on its own between two frames.
//!
//! The library it runs against is [`support::Fixture`]; twenty seconds of
//! audio and two clips is the smallest shape these assertions can be exact
//! over.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

/// Long enough that the drag has room to extend a clip into it, short enough
/// that decoding and rendering it is a fraction of a second.
const TRACK_SECONDS: u32 = 20;

/// Two clips, on two lanes.
const CLIPS: [(&str, &str, f64, f64, i64); 2] = [
    ("pattern-strobe", "Strobe", 2.0, 6.0, 0),
    ("pattern-wash", "Wash", 8.0, 14.0, 1),
];

/// How far the drag pulls the clip's end, in logical pixels. The timeline
/// opens at 50 px/s, so this is a known number of seconds — which is what lets
/// the assertion be exact rather than approximate.
const DRAG_X: f64 = 100.;

fn harness() -> Harness {
    Fixture::new(
        "track-editor",
        TRACK_SECONDS,
        CLIPS
            .iter()
            .map(|(pattern, name, start, end, z)| Clip::new(*pattern, *name, *start, *end).lane(*z))
            .collect(),
    )
    .open(Mode::Headless)
}

/// Open the track, read the timeline, drag a clip's end, read it again, then
/// leave and come back and read it a third time. Then play, and read where the
/// playhead got to.
///
/// Every reading is the geometry the canvas drew beside the toolbar's own
/// account of the same screen, so a drag that only moved the picture and a
/// play that only changed a label both fail.
const SCRIPT: &str = r#"
    function open() {
        nav.track("Aurora");
        // The editor's opening load is five commands on a runtime gpui does
        // not own, one of which decodes and renders twenty seconds of audio.
        // Waited for by its result (the timeline's waveform card), not by a
        // frame count: nav.track returns as soon as the row was pressable,
        // which is earlier than the old hand-rolled walk got here, so a bare
        // frames(20) can land inside the load.
        until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
        nav.expand();
        // A test about the editor's own geometry, so give it the whole column:
        // the stage above it would otherwise take 40% of the height.
        nav.stageOff();
        return read();
    }

    function read() {
        const shot = app.snapshot();
        const clips = {};
        for (const card of shot.findAll({ role: "card" })) {
            clips[card.label] = { x: card.bounds.x, width: card.bounds.width };
        }
        const playhead = shot.find({ role: "slider", label: "Playhead" });
        return {
            clips,
            playhead: playhead === undefined ? null : playhead.bounds.x,
            transport: shot.findAll({ role: "button" }).map((n) => n.label),
            status: shot.findAll({ role: "text" }).map((n) => n.label),
        };
    }

    nav.venue("Test Venue");
    app.frames(8);
    const opened = open();

    // Drag the *handle*, not the clip: the harness starts a drag from a node's
    // centre, and a clip's centre is nowhere near either of its edges.
    app.drag(app.snapshot().find({ role: "slider", label: "Strobe end" }), { dx: 100, dy: 0 });
    app.frames(20);
    const moved = read();

    // Leave the screen entirely and come back. A repaint survives the first
    // reading; only a write survives this one.
    nav.closeTab();
    app.frames(6);
    const reopened = open();

    // Waited in milliseconds, not in frames. The playhead is advanced by a
    // wall clock and re-read on a 33 ms poll, so what it takes to move is
    // *time* — a frame count only stood in for that while a frame happened to
    // be slow.
    //
    // But *starting* is not a wait at all: Play is two commands on a runtime
    // gpui does not own, and how long that round trip takes depends on what
    // else is running on the machine, so any fixed wait passes on an idle box
    // and fails on a busy one. Wait for the transport to say it started, then
    // wait a little time for it to have moved. Bounded, so a transport that
    // never starts fails with a reason rather than hanging — and it stops as
    // soon as it is playing, which is what keeps a twenty-second track from
    // reaching its end and stopping again.
    function waitFor(label, limit) {
        for (let i = 0; i < limit; i++) {
            if (app.snapshot().find({ role: "button", label }) !== undefined) return true;
            app.frames(1, { waitMs: 60 });
        }
        return false;
    }

    app.click(app.snapshot().find({ role: "button", label: "Play" }));
    const started = waitFor("Pause", 30);
    // The label flips on the first poll that reads `isPlaying`, and that poll
    // can still read a position rounding to zero. A pixel is 20 ms at this
    // zoom, so a few polls is several of them.
    app.frames(4, { waitMs: 60 });
    const playing = read();
    app.click(app.snapshot().find({ role: "button", label: "Pause" }));
    app.frames(4, { waitMs: 60 });
    const paused = read();

    ({ opened, moved, reopened, playing, paused, started })
"#;

#[test]
fn a_clip_edge_dragged_on_the_timeline_moves_stays_moved_and_the_playhead_runs() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // 1. The timeline names its scrubbing surface and every clip on it.
    let opened = &out["opened"];
    assert!(
        status(opened).contains(&"2 CLIPS".to_string()),
        "{opened:#}"
    );
    assert!(
        opened["clips"]["Waveform"].is_object(),
        "the canvas did not name the waveform region: {opened:#}"
    );
    for (_, label, ..) in CLIPS {
        assert!(
            opened["clips"][label].is_object(),
            "the canvas did not name a clip for {label}: {opened:#}"
        );
    }

    // 2. The drag lengthened exactly the clip it took hold of, by exactly the
    //    drag distance, and left its start and every other clip alone.
    let moved = &out["moved"];
    assert_eq!(
        width(moved, "Strobe") - width(opened, "Strobe"),
        DRAG_X,
        "the clip's end did not move by the drag distance"
    );
    assert_eq!(x(moved, "Strobe"), x(opened, "Strobe"), "the start drifted");
    assert_eq!(
        (x(moved, "Wash"), width(moved, "Wash")),
        (x(opened, "Wash"), width(opened, "Wash")),
        "Wash moved, and it was not the one dragged"
    );

    // 3. Reopening re-reads the score, so what comes back is what was written
    //    — not what was still on screen.
    let reopened = &out["reopened"];
    assert!(
        status(reopened).contains(&"2 CLIPS".to_string()),
        "{reopened:#}"
    );
    for (_, label, ..) in CLIPS {
        assert_eq!(
            (x(reopened, label), width(reopened, label)),
            (x(moved, label), width(moved, label)),
            "{label} came back from the score in a different place"
        );
    }

    // 4. Playing moves the playhead on its own, and the button says so. There
    //    is no sound to assert on headless — the transport's position is the
    //    whole observable.
    assert!(
        transport(&out["opened"]).contains(&"Play".to_string()),
        "the transport should offer Play before anything is playing"
    );
    assert_eq!(
        out["started"], true,
        "the transport never reported playing: {:#}",
        out["playing"]
    );
    let (before, after) = (playhead(reopened), playhead(&out["playing"]));
    assert!(
        after > before,
        "the playhead did not advance: {before} -> {after}; {:#}",
        out["playing"]
    );
    assert!(
        transport(&out["playing"]).contains(&"Pause".to_string()),
        "the transport should offer Pause while playing: {:#}",
        out["playing"]
    );
    assert!(
        transport(&out["paused"]).contains(&"Play".to_string()),
        "the transport should offer Play again once paused"
    );
}

fn x(reading: &Value, label: &str) -> f64 {
    reading["clips"][label]["x"]
        .as_f64()
        .unwrap_or_else(|| panic!("{label} has no x: {reading:#}"))
}

fn width(reading: &Value, label: &str) -> f64 {
    reading["clips"][label]["width"]
        .as_f64()
        .unwrap_or_else(|| panic!("{label} has no width: {reading:#}"))
}

fn playhead(reading: &Value) -> f64 {
    reading["playhead"]
        .as_f64()
        .unwrap_or_else(|| panic!("no playhead in {reading:#}"))
}

fn status(reading: &Value) -> Vec<String> {
    serde_json::from_value(reading["status"].clone()).expect("a reading has status text")
}

fn transport(reading: &Value) -> Vec<String> {
    serde_json::from_value(reading["transport"].clone()).expect("a reading has transport buttons")
}
