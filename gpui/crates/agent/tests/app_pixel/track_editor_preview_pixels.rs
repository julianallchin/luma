//! The heatmap in a clip's body is a picture of the pattern, not a tint.
//!
//! The fixture's lit graph is "every fixture, red, pulsing once a beat", so
//! the honest evidence that the body paints the *heatmap* — and not the flat
//! pattern-colour fallback it degrades to — is horizontal structure: a pulse
//! sweeps a row of the body through many red levels, where the flat fill is
//! one colour interrupted only by the beat lines under it. A property test
//! rather than a golden image, like its neighbours: the picture is
//! deterministic (fixture beats, fixture rig, a closed-form envelope), but
//! what it must *keep* under any refactor is redness and per-cell variation,
//! not byte equality.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test app_pixel track_editor_preview
//! ```

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::collections::HashSet;
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 8;

/// The same shape as `headless/track_editor_previews.rs`, on a renderer: a
/// rigged venue so the pattern has fixtures to light, and lit clips long
/// enough to hold several pulses — three of them across two lanes, so the
/// full-window shot shows previews as the timeline actually wears them.
fn harness() -> Harness {
    Fixture::new(
        "track-editor-preview-pixels",
        TRACK_SECONDS,
        vec![
            Clip::new("pattern-pulse", "Pulse", 0.5, 4.5).lit(),
            Clip::new("pattern-sweep", "Sweep", 5.0, 7.5).lit(),
            Clip::new("pattern-strobe", "Strobe", 1.5, 6.0)
                .lit()
                .lane(1),
        ],
    )
    .with_rig()
    // Taller than the default 800: the lane stack is bottom-anchored to the
    // canvas floor, and the extra height keeps the whole stack — floor line
    // included — clear of the window edge in the kept full-window shot.
    .window(1480., 1000.)
    .open(Mode::Pixel)
}

const SCRIPT: &str = r#"
    nav.trackEditor("Test Venue", "Aurora");
    nav.expand();
    nav.stageOff();
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
    // The previews land when the seam's render does — one pattern evaluation
    // per clip after the timeline is already up.
    for (const label of ["Pulse preview", "Sweep preview", "Strobe preview"]) {
        until(label, (s) => s.find({ role: "card", label }) !== undefined);
    }
    app.frames(8, { waitMs: 30 });
    // Select one clip, so the shot holds both alphas: an opaque selected body
    // beside the translucent rest.
    app.click(app.snapshot().find({ role: "card", label: "Sweep" }));
    app.frames(8, { waitMs: 30 });
    const preview = app.snapshot().find({ role: "card", label: "Pulse preview" });
    ({ shot: app.screenshot({ node: preview }), window: app.screenshot() })
"#;

#[test]
fn the_clip_body_paints_the_heatmap_rather_than_a_flat_fill() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    let (path, image) = support::image::keep_in("clip-preview", &out["shot"], "clip-body");
    // Kept beside the body shot, for eyes rather than assertions: the whole
    // timeline wearing its previews, one clip selected.
    let (window, _) = support::image::keep_in("clip-preview", &out["window"], "timeline");
    eprintln!(
        "clip preview shots:\n  body: {}\n  window: {}",
        path.display(),
        window.display()
    );

    // Rows near the edges catch the clip border and the header hairline; the
    // middle of the body is pure heatmap over the lane bed.
    let y = image.height() / 2;
    let mut reds = 0u32;
    let mut levels: HashSet<[u8; 4]> = HashSet::new();
    for x in 0..image.width() {
        let [r, g, b, _] = image.get_pixel(x, y).0;
        levels.insert(image.get_pixel(x, y).0);
        if r > g.saturating_add(16) && r > b.saturating_add(16) {
            reds += 1;
        }
    }

    // The graph lights every fixture red, so a heatmap row is red wherever a
    // pulse is up — a body with next to no red pixels painted something else.
    assert!(
        reds as f64 > f64::from(image.width()) * 0.2,
        "only {reds} of {} centre-row pixels are red-dominant — this does not \
         look like the red pulse's heatmap\n  {}",
        image.width(),
        path.display(),
    );

    // A pulse decays through many levels per beat; the flat fallback fill is
    // one colour with beat lines through it, which is a handful. The margin is
    // wide because the exact count depends on the envelope and the zoom —
    // what cannot happen is a flat body clearing it.
    assert!(
        levels.len() > 12,
        "the centre row holds only {} distinct colours — a flat fill, not a \
         heatmap\n  {}",
        levels.len(),
        path.display(),
    );
}
