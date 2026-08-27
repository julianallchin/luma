//! A clip whose pattern renders a heatmap reports a preview surface.
//!
//! The clip body is inert to the pointer, so the only way a script can tell a
//! heatmap-bearing body from the flat fallback fill is the `"<pattern>
//! preview"` node the canvas registers exactly when a decoded preview exists.
//! One lit clip and one bare one would not sharpen this: the seam renders a
//! track's previews in one command, and a pattern with no graph fails that
//! command for the whole track — so the discriminating fixture is a lit track
//! against the suite's many unlit ones, whose editors must keep showing *no*
//! preview nodes (asserted where those suites enumerate cards, by their
//! assertions not having changed).
//!
//! The preview arrives on its own schedule — the seam evaluates the pattern
//! over the clip's span after the timeline is already up — so the test waits
//! for the node by name rather than counting frames.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 8;

/// A rigged venue, because the heatmap's rows are the pattern's primitives:
/// a lit graph over a venue with nothing patched evaluates to an empty
/// preview, and an empty preview is indistinguishable from a slow one.
fn harness() -> Harness {
    Fixture::new(
        "track-editor-previews",
        TRACK_SECONDS,
        vec![Clip::new("pattern-pulse", "Pulse", 0.5, 4.5).lit()],
    )
    .with_rig()
    .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    nav.trackEditor("Test Venue", "Aurora");
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);

    // The clip itself is up as soon as the clip list lands...
    until("the clip", (s) => s.find({ role: "card", label: "Pulse" }) !== undefined);
    // ...and its preview surface appears when the seam's render lands, which
    // is a pattern evaluation later.
    until("the preview surface", (s) =>
        s.find({ role: "card", label: "Pulse preview" }) !== undefined);

    const shot = app.snapshot();
    const clip = shot.find({ role: "card", label: "Pulse" });
    const preview = shot.find({ role: "card", label: "Pulse preview" });
    ({
        clip: clip.bounds,
        preview: preview.bounds,
    })
"#;

#[test]
fn a_lit_clip_reports_a_preview_surface_under_its_header() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(120));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let (clip, preview) = (&out["clip"], &out["preview"]);
    let field = |value: &Value, name: &str| {
        value[name]
            .as_f64()
            .unwrap_or_else(|| panic!("no {name} in {value:#}"))
    };

    // The preview surface is the clip's body: the same span, directly under
    // the 18px header the pointer owns.
    assert_eq!(field(preview, "x"), field(clip, "x"), "{out:#}");
    assert_eq!(field(preview, "width"), field(clip, "width"), "{out:#}");
    assert_eq!(
        field(preview, "y"),
        field(clip, "y") + 18.,
        "the preview surface does not start under the clip header: {out:#}"
    );
    assert!(
        field(preview, "height") > 0.,
        "the preview surface has no height: {out:#}"
    );
}
