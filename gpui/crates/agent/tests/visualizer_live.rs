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
//! Pixel-only, and one test to a binary, for the reasons `visualizer.rs` gives.
#![cfg(feature = "pixel")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture, NAV, TRACK_NAME, VENUE_NAME};

/// The whole track, lit. The clip spans the track so that whatever time the
/// transport is at is inside it — a gap would make an unlit frame a legitimate
/// outcome and the test unable to tell that from a broken one.
const SECONDS: u32 = 20;

fn harness() -> Harness {
    Fixture::new(
        "visualizer-live",
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

fn differing_fraction(a: &image::RgbaImage, b: &image::RgbaImage) -> f32 {
    assert_eq!(a.dimensions(), b.dimensions(), "shots are different sizes");
    let differing = a
        .pixels()
        .zip(b.pixels())
        .filter(|(p, q)| (0..3).any(|c| i32::from(p[c]).abs_diff(i32::from(q[c])) > 8))
        .count();
    differing as f32 / (a.width() * a.height()) as f32
}

/// The gate: the light in the picture came from the score, and follows the
/// playhead.
#[test]
fn the_rig_is_lit_by_the_playing_track() {
    let mut harness = harness();

    // The 3D view is opened over the *track editor*, which is the screen that
    // knows a `(track, venue)` — and so the only one whose open composites a
    // score onto the rig at all.
    run(
        &mut harness,
        &format!(
            r#"
            {NAV}
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            // The clip is the evidence the *score* has landed, and the score
            // is what `open_visualizer` needs: with none in hand the track
            // editor has no `(track, venue)` subject and the action is a
            // silent no-op.
            until("the clip", (s) => s.find({{ role: "card", label: "Pulse" }}) !== undefined);
            app.action("luma::OpenVisualizer");
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

    let first = pixels(&run(&mut harness, "app.screenshot()"));
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
            // The view's own transport, not `luma::PlayPause`: that action is
            // scoped to the track editor, and this screen reads the host clock
            // rather than the editor's playhead.
            nav.step("the Play button", "button", "Play");
            app.frames(20, { waitMs: 55 });
        "#,
    );
    let second = pixels(&run(&mut harness, "app.screenshot()"));

    let moved = differing_fraction(&first, &second);
    assert!(
        moved > 0.01,
        "a second of playback changed {:.3}% of the frame — the light is not \
         following the playhead",
        moved * 100.0
    );
}
