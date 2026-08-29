//! The rig is lit by the track, and the light moves with it.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test visualizer_live
//! ```
//!
//! `visualizer.rs` proves the viewport draws a rig and redraws it when the
//! camera moves. That is geometry, and geometry would pass with the evaluator
//! disconnected entirely — a dark stage of grey movers is a perfectly
//! respectable picture of nothing. This is the other half: that the frame's
//! colour comes from `Scene::render` sampled at the transport's time, through
//! `Library::sample_universe` and `build_frame_with`, and therefore changes
//! when the transport does.
//!
//! Both halves are needed and neither implies the other, which is why they are
//! two tests and not one with two assertions.
//!
//! Pixel-only for the reasons `visualizer.rs` gives, and one test to a
//! *fixture* for the reason `visualizer_budget.rs` gives — a fixture name is a
//! config directory, and two tests sharing one seed the same library at once.
#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture, NAV, TRACK_NAME, VENUE_NAME};

/// The whole track, lit. The clip spans the track so that whatever time the
/// transport is at is inside it — a gap would make an unlit frame a legitimate
/// outcome and the test unable to tell that from a broken one.
const SECONDS: u32 = 20;

/// A library of this fixture's own, named by the test that seeds it.
///
/// One test to a fixture, without exception: the name *is* the config
/// directory, so two tests sharing one would seed the same library
/// concurrently and race each other's venue insert.
fn harness(fixture: &'static str) -> Harness {
    Fixture::new(
        fixture,
        SECONDS,
        vec![Clip::new("pattern-pulse", "Pulse", 0., f64::from(SECONDS)).lit()],
    )
    .with_rig()
    .open(Mode::Pixel)
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

/// The stage's own pixels, not the window's.
///
/// The stage rides *above* an editor now rather than filling a tab, so it is
/// about a quarter of the window and the rest is the track editor's chrome.
/// Measuring the whole frame would break both assertions below, in opposite
/// directions: the beam's colour gets diluted into all that neutral UI until
/// red no longer leads, and — worse — the editor's own playhead sweeping across
/// its timeline would satisfy "the frame changed between shots" while the light
/// stood perfectly still. Cropping to the viewport is what keeps each
/// measurement about the thing it names.
const STAGE_SHOT: &str =
    r#"app.screenshot({ node: app.snapshot().find({ role: "card", label: "Stage" }) })"#;

fn pixels(shot: &Value) -> image::RgbaImage {
    let path = shot["path"].as_str().expect("a screenshot has a path");
    image::open(path)
        .unwrap_or_else(|error| panic!("could not read {path}: {error}"))
        .to_rgba8()
}

/// How red the frame is, in mean channel levels: `(red, other)`.
///
/// The rig, the grid and the room are all neutral greys, and the only saturated
/// hue anywhere in this fixture is the `color` node's pure red. So a red mean
/// meaningfully above the green/blue mean can only be beam.
fn redness(image: &image::RgbaImage) -> (f32, f32) {
    let n = f64::from(image.width() * image.height());
    let (red, other) = image.pixels().fold((0.0f64, 0.0f64), |(r, o), p| {
        (
            r + f64::from(p[0]),
            o + f64::from(p[1]) / 2.0 + f64::from(p[2]) / 2.0,
        )
    });
    ((red / n) as f32, (other / n) as f32)
}

/// The shared diff at the shared noise floor — see `support::image`.
fn differing_fraction(left: &image::RgbaImage, right: &image::RgbaImage) -> f32 {
    support::image::differing_fraction(left, right, support::image::CHANNEL_NOISE)
}

/// The gate: the light in the picture came from the score, and follows the
/// playhead.
#[test]
fn the_rig_is_lit_by_the_playing_track() {
    let mut harness = harness("visualizer-live-playback");

    // The stage is a view of the tab below it, and the *track editor* is the
    // only tab that names a `(track, venue)` — so opening one is what puts a
    // stage up with a score composited onto its rig at all.
    run(
        &mut harness,
        &format!(
            r#"
            {NAV}
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            // The clip is the evidence the *score* has landed, and the score
            // is what lights the stage: until the editor has a `(track, venue)`
            // subject the stage above it is up but unlit.
            until("the clip", (s) => s.find({{ role: "card", label: "Pulse" }}) !== undefined);
            nav.expand();
            app.frames(10, {{ waitMs: 60 }});
        "#
        ),
    );

    // The toolbar's own account of itself. `UNLIT` is what this screen shows
    // when the sample comes back empty, so reading it here separates "the
    // compositor installed nothing" from "the renderer drew it wrong" — two
    // failures that look identical in the pixels.
    run(
        &mut harness,
        r#"
            const readout = (s) =>
                s.find((n) => n.role === "text" && n.label.includes("FIXTURES"));
            const shot = until("the rig's readout", (s) => readout(s) !== undefined);
            const label = readout(shot).label;
            if (!label.includes("LIVE")) {
                throw new Error(`nothing is composited onto the rig: ${label}`);
            }
        "#,
    );

    let first = pixels(&run(&mut harness, STAGE_SHOT));
    let (red, other) = redness(&first);
    assert!(
        red > other + 2.0,
        "the frame carries no beam colour (red {red:.2} vs {other:.2}) — \
         the evaluated universe is not reaching the renderer"
    );

    // Then let the transport run. The pattern is a sine over the beat grid with
    // a two-second period, so a second of playback is half a cycle: if the
    // sample were pinned to one time, or the frame cached, this cannot move.
    run(
        &mut harness,
        r#"
            // The editor's transport, which is now the only one: it drives the
            // host clock, and the host clock is what this stage samples. The
            // stage's own Play button was the same verb spelled twice.
            nav.step("the Play button", "button", "Play");
            app.frames(20, { waitMs: 55 });
        "#,
    );
    let second = pixels(&run(&mut harness, STAGE_SHOT));

    let moved = differing_fraction(&first, &second);
    assert!(
        moved > 0.01,
        "a second of playback changed {:.3}% of the frame — the light is not \
         following the playhead",
        moved * 100.0
    );
}

/// The other direction: the score does not only light the rig, an *edit* to it
/// relights the rig.
///
/// A scene is installed by a command, so a screen that edited clips without
/// re-installing would pass the test above and still show a rig that answered
/// the document as it was when the view opened — every colour, every span,
/// every selection frozen at open. That is exactly what this host did before
/// `track_editor::sync_composite`.
///
/// `selection` is the arg to move because it is the only one this fixture's
/// graph actually consumes (`support`'s `light`): pointing it at a group the
/// venue does not have leaves the pattern with no fixtures to apply to, so the
/// beam has to go out. A dimmer beam would be ambiguous; a dark rig is not.
#[test]
fn an_arg_edit_relights_the_rig() {
    let mut harness = harness("visualizer-live-arg-edit");

    run(
        &mut harness,
        &format!(
            r#"
            {NAV}
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            until("the clip", (s) => s.find({{ role: "card", label: "Pulse" }}) !== undefined);
            nav.expand();
            app.frames(10, {{ waitMs: 60 }});
            // Selecting the clip is what populates the args sheet; the schema
            // is a round trip, so the expression field arriving is the signal
            // that there is something to edit.
            app.click(app.snapshot().find({{ role: "card", label: "Pulse" }}));
            until("the strip's selection field", (s) =>
                s.findAll({{ role: "input" }}).some((n) => n.label.startsWith("expression = ")));
            app.frames(6, {{ waitMs: 40 }});
        "#
        ),
    );

    let lit = pixels(&run(&mut harness, STAGE_SHOT));
    let (red, other) = redness(&lit);
    assert!(
        red > other + 2.0,
        "the rig was not lit before the edit (red {red:.2} vs {other:.2}) — \
         this test cannot say anything about the edit"
    );

    run(
        &mut harness,
        r#"
            const field = () => app.snapshot().findAll({ role: "input" })
                .find((n) => n.label.startsWith("expression = "));
            app.click(field());
            app.key("cmd-a backspace");
            // A group no venue in this fixture has. Characters go through
            // `app.type` because a bare keystroke is not text input.
            app.type(field(), "nothing_is_in_this_group", { restale: "match" });
            // Enter with the suggestion menu up takes the suggestion rather
            // than committing, and an unknown word suggests nothing — so this
            // is the commit. The composite then trails the edit by a round
            // trip.
            app.key("enter");
            app.frames(20, { waitMs: 60 });
        "#,
    );

    let dark = pixels(&run(&mut harness, STAGE_SHOT));
    let (red_after, other_after) = redness(&dark);
    assert!(
        red_after <= other_after + 1.0,
        "the rig is still red after the selection was pointed at nothing \
         (red {red_after:.2} vs {other_after:.2}, was {red:.2} vs {other:.2}) — \
         the edit never reached the render engine's scene"
    );
}
