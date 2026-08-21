//! What the waveform is drawn from, at each end of the zoom range.
//!
//! The stored envelope is `FULL_WAVEFORM_SIZE` buckets however long the track
//! is, so on a long enough track a deep zoom puts less than one of them under
//! each pixel and the picture becomes a staircase of a bucket that was averaged
//! at import. Past that point the editor measures the visible range instead —
//! `get_track_waveform_window`, a bucket per pixel — and this is the test that
//! it actually does, and only there.
//!
//! The evidence is the panel's own resolution readout. It is not a debug label:
//! it is computed by the same `drawn_buckets` the canvas asks before choosing
//! its source, so a readout that says FINE and a canvas drawing the stored
//! envelope cannot both happen. The number in it is the bucket count of the
//! window in hand, which is what "a bucket per pixel" is a claim about.

#![cfg(feature = "app")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

/// Three minutes. `FULL_WAVEFORM_SIZE / 180` is 167 buckets a second, so the
/// stored envelope is enough at the opening zoom of 50 px/s and not enough at
/// the 500 px/s the wheel stops at — which is the only length at which both
/// halves of this test can be true of one track.
const TRACK_SECONDS: u32 = 180;

/// `View::MIN_ZOOM` / `View::MAX_ZOOM` in `track_editor.rs`, in pixels per
/// second. Duplicated rather than exported for the same reason the budget test
/// duplicates the minimum: the editor's zoom limits are its own business and
/// this test only needs to recognise the states.
const MAX_ZOOM: f64 = 500.;

/// Where the fixture's one clip sits, and how long it is.
///
/// A zoom is read back off a clip's drawn width, which only works while the
/// whole clip is on screen — so it is a second long, and it sits under the
/// middle of the opening view. The wheel zooms about the pointer and the
/// pointer is the middle of the canvas, so a clip that starts centred stays
/// centred: 1200 px at the opening 50 px/s shows 0..24 s, and at 500 px/s it
/// shows 2.4 s of the same middle.
const CLIP: (f64, f64) = (11.5, 1.);

fn harness() -> Harness {
    Fixture::new(
        "track-editor-waveform",
        TRACK_SECONDS,
        vec![Clip::new(
            "pattern-strobe",
            "Strobe",
            CLIP.0,
            CLIP.0 + CLIP.1,
        )],
    )
    .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    function status() {
        return app.snapshot().findAll({ role: "text" }).map((n) => n.label);
    }

    function fine() {
        return status().find((label) => label.startsWith("FINE ")) ?? null;
    }

    function waveform() {
        return app.snapshot().find({ role: "card", label: "Waveform" });
    }

    /** Wait until `check` returns something truthy, or give up and say so. */
    function settle(check, limit) {
        for (let i = 0; i < limit; i++) {
            const value = check();
            if (value) return value;
            app.frames(1, { waitMs: 60 });
        }
        return null;
    }

    app.click(app.snapshot().find({ role: "card", label: "Test Venue" }));
    app.frames(8);
    app.click(app.snapshot().find({ role: "row", label: "Aurora" }));

    // Three minutes of audio to decode and render an envelope for, on a runtime
    // gpui does not own — waited for by its result rather than by a frame count.
    const opened = settle(waveform, 200);
    const width = opened === null ? 0 : opened.bounds.width;

    // At the opening zoom the stored envelope still has a bucket per pixel, so
    // nothing should have been measured. Given time to be wrong about that.
    app.frames(10, { waitMs: 60 });
    const coarse = fine();

    // All the way in. The zoom is exponential in the wheel distance and clamps
    // at MAX_ZOOM, so this overshoots deliberately.
    app.scroll(waveform(), { dy: 2000, steps: 20, modifiers: ["platform"] });
    const zoomedIn = settle(fine, 100);

    // A clip of known length reads the zoom back off its drawn width.
    const clip = app.snapshot().find({ role: "card", label: "Strobe" });
    const zoom = clip.bounds.width / CLIP_SECONDS;

    // And all the way back out, where there is nothing left to measure.
    app.scroll(waveform(), { dy: -2400, steps: 20, modifiers: ["platform"] });
    settle(() => fine() === null, 100);

    ({
        width,
        coarse,
        zoomedIn,
        zoom,
        stillFine: fine(),
    })
"#;

#[test]
fn a_deep_zoom_repaints_from_a_measured_window_and_a_zoom_out_gives_it_back() {
    let mut harness = harness();
    let script = SCRIPT.replace("CLIP_SECONDS", &CLIP.1.to_string());
    let result = harness.exec(&script, Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let width = out["width"].as_f64().unwrap_or(0.);
    assert!(
        width > 100.,
        "the editor never drew a waveform to zoom into: {out:#}"
    );
    assert_eq!(
        out["coarse"],
        Value::Null,
        "the stored envelope still has a bucket per pixel at the opening zoom, \
         so nothing should have been measured: {out:#}"
    );
    assert_eq!(
        out["zoom"].as_f64(),
        Some(MAX_ZOOM),
        "the timeline is not at full zoom-in: {out:#}"
    );

    // A bucket per pixel across the view, and the margin either side of it that
    // makes a pan free — so strictly more buckets than the canvas is wide.
    let buckets = out["zoomedIn"]
        .as_str()
        .and_then(|label| label.strip_prefix("FINE "))
        .and_then(|count| count.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("the deep zoom never repainted from a window: {out:#}"));
    assert!(
        buckets >= width,
        "{buckets} buckets over a {width}px canvas is less than a bucket a pixel: {out:#}"
    );

    assert_eq!(
        out["stillFine"],
        Value::Null,
        "zoomed back out, the editor is still drawing a measured window it no \
         longer needs: {out:#}"
    );
}
