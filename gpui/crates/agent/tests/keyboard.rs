//! The app's keyboard, end to end: a key, an action, and a text field that
//! keeps the keys it is typing.
//!
//! The three things this pins are the three that can silently stop working.
//! *Routing*: an action only reaches a handler if something on screen holds
//! focus, so pressing space has to move the transport and not merely be
//! swallowed. *Naming*: `app.action("luma::PlayPause")` and the space bar must
//! arrive at the same place, or a script is driving an app no person could.
//! *Precedence*: gpui matches key bindings before it delivers key events, so a
//! focused text field cannot defend its own space bar — only the binding's
//! `!TextInput` scope can, and nothing but a test says whether it does.
//!
//! Playback is asserted the same way [`track_editor`] asserts it: on headless
//! there is no sound, so the only honest evidence that space started the
//! transport is that the playhead moved on its own between two readings.

#![cfg(feature = "app")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 20;

fn harness() -> Harness {
    Fixture::new(
        "keyboard",
        TRACK_SECONDS,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0).lane(0)],
    )
    .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    function read() {
        const shot = app.snapshot();
        const playhead = shot.find({ role: "slider", label: "Playhead" });
        const search = shot.find({ role: "input" });
        return {
            playhead: playhead === undefined ? null : playhead.bounds.x,
            transport: shot.findAll({ role: "button" }).map((n) => n.label),
            text: shot.findAll({ role: "text" }).map((n) => n.label),
            search: search === undefined ? null : { label: search.label, focused: search.focused },
            // The venue grid draws a card per venue; every other screen is
            // "not the venue grid" exactly when that card is absent.
            onVenueGrid: shot.find({ role: "card", label: "Test Venue" }) !== undefined,
        };
    }

    // Bounded, because a transport that never starts should fail with a reason
    // rather than hang. Same shape as the track-editor gate: how long a play
    // takes to land is a round trip on a runtime gpui does not own.
    function waitFor(label, limit) {
        for (let i = 0; i < limit; i++) {
            if (app.snapshot().find({ role: "button", label }) !== undefined) return true;
            app.frames(1, { waitMs: 60 });
        }
        return false;
    }

    function openEditor() {
        nav.track("Aurora");
        // The opening load is five commands, one of which decodes and renders
        // twenty seconds of audio — waited for by its result (the timeline's
        // waveform card), not by a frame count: nav.track returns as soon as
        // the row was pressable, which is earlier than the old hand-rolled
        // walk got here, so a bare frames(20) can land inside the load.
        until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
        nav.expand();
        return read();
    }

    // 0. Before anything has been clicked. A window whose screens focus
    //    nothing has no dispatch path to route an action along, so this is the
    //    reading that says the venue grid took the keyboard on its own.
    app.key("secondary-,");
    app.frames(6);
    const cold = read();
    app.key("escape");
    app.frames(6);

    nav.venue("Test Venue");
    app.frames(8);
    const opened = openEditor();

    // 1. Space plays. Nothing was clicked, so this is focus routing a binding.
    app.key("space");
    const started = waitFor("Pause", 30);
    app.frames(4, { waitMs: 60 });
    const playing = read();

    // 2. Space again stops it — and a stopped playhead stays put across a wait
    //    that a running one would have crossed several pixels in.
    app.key("space");
    const stopped = waitFor("Play", 30);
    const paused = read();
    app.frames(6, { waitMs: 60 });
    const stillPaused = read();

    // 3. The same verb by name. This is the path a script takes when it does
    //    not want to know which key happens to be bound to it.
    app.action("luma::PlayPause");
    const restarted = waitFor("Pause", 30);
    app.key("space");
    waitFor("Play", 30);

    // 4. Escape is Back, and Back from the editor is the browser it opened
    //    from.
    app.key("escape");
    app.frames(6);
    const wentBack = read();

    // 5. The search field takes the keyboard, and then keeps it: space is a
    //    space and escape clears the query rather than leaving the venue.
    app.click(app.snapshot().find({ role: "input" }));
    app.frames(2);
    const focused = read();
    app.key("space");
    app.frames(2);
    const typed = read();
    app.key("escape");
    app.frames(2);
    const cleared = read();

    // 6. And nothing the search field was sent reached the transport: reopen
    //    the editor and ask the audio host what it is doing.
    const reopened = openEditor();

    // 7. The macOS convention, over whatever is showing, and out again.
    app.key("secondary-,");
    app.frames(6);
    const settings = read();
    app.key("escape");
    app.frames(6);
    const closed = read();

    ({
        cold, opened, playing, paused, stillPaused, wentBack, focused, typed, cleared,
        reopened, settings, closed, started, stopped, restarted,
    })
"#;

#[test]
fn keys_and_actions_route_to_the_focused_screen_and_a_text_field_keeps_its_own() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // 0. The venue grid held the keyboard before a single click.
    assert!(
        text(&out["cold"]).contains(&"SETTINGS".to_string()),
        "no screen was focused at launch, so cmd-, routed nowhere: {:#}",
        out["cold"]
    );

    // 1. Space started the transport, and the playhead moved on its own.
    assert!(
        transport(&out["opened"]).contains(&"Play".to_string()),
        "the editor should offer Play before anything is playing: {:#}",
        out["opened"]
    );
    assert_eq!(
        out["started"], true,
        "space never started the transport — nothing is focused, or nothing is bound: {:#}",
        out["playing"]
    );
    assert!(
        playhead(&out["playing"]) > playhead(&out["opened"]),
        "the playhead did not advance after space: {:#} -> {:#}",
        out["opened"],
        out["playing"]
    );

    // 2. Space again stopped it, and it stayed stopped.
    assert_eq!(out["stopped"], true, "space did not stop the transport");
    assert_eq!(
        playhead(&out["stillPaused"]),
        playhead(&out["paused"]),
        "the playhead kept moving after space stopped the transport"
    );

    // 3. The action by name is the same verb.
    assert_eq!(
        out["restarted"], true,
        "`luma::PlayPause` did not reach the transport"
    );

    // 4. Escape left the editor for the browser it was opened from.
    let back = &out["wentBack"];
    assert_eq!(
        back["onVenueGrid"], false,
        "escape went further back than the browser: {back:#}"
    );
    assert!(
        back["search"].is_object(),
        "escape did not land on the track browser: {back:#}"
    );

    // 5. The focused field kept both keys: space became text, escape cleared
    //    the query instead of dispatching Back.
    assert_eq!(
        out["focused"]["search"]["focused"], true,
        "clicking the search field did not focus it: {:#}",
        out["focused"]
    );
    assert_eq!(
        out["typed"]["search"]["label"], " ",
        "space in the search field did not type a space: {:#}",
        out["typed"]
    );
    assert_eq!(
        out["cleared"]["search"]["label"], "Search tracks…",
        "escape in the search field did not clear the query: {:#}",
        out["cleared"]
    );
    for reading in ["typed", "cleared"] {
        assert_eq!(
            out[reading]["onVenueGrid"], false,
            "a key meant for the search field navigated instead: {:#}",
            out[reading]
        );
    }

    // 6. …and the transport is where it was left: stopped.
    assert!(
        transport(&out["reopened"]).contains(&"Play".to_string()),
        "the search field's space bar reached the transport: {:#}",
        out["reopened"]
    );

    // 7. `cmd-,` opened settings over the editor, and escape gave it back.
    assert!(
        text(&out["settings"]).contains(&"SETTINGS".to_string()),
        "cmd-, did not open settings: {:#}",
        out["settings"]
    );
    assert!(
        text(&out["closed"]).contains(&"Aurora".to_string()),
        "escape did not return to the screen settings covered: {:#}",
        out["closed"]
    );
}

fn playhead(reading: &Value) -> f64 {
    reading["playhead"]
        .as_f64()
        .unwrap_or_else(|| panic!("no playhead in {reading:#}"))
}

fn transport(reading: &Value) -> Vec<String> {
    serde_json::from_value(reading["transport"].clone()).expect("a reading has buttons")
}

fn text(reading: &Value) -> Vec<String> {
    serde_json::from_value(reading["text"].clone()).expect("a reading has text")
}
