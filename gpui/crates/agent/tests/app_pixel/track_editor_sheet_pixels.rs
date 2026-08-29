//! The args sheet under a real renderer: closed, mid-flight, open, and
//! retargeted, shot from the same session.
//!
//! What the pixels are for is the *slide*. The node protocol can say the sheet
//! is present and where its box is; only a frame can say that a sheet caught
//! part-way in is genuinely part-way in — narrower than the settled one and
//! wider than nothing — and that retargeting repaints its interior without
//! moving it. The closed shot is the control: the timeline with no sheet over
//! it at all, which is the thing the bottom strip could never be.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test app_pixel track_editor_sheet
//! ```

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 20;

/// The headless sheet fixture's shape, on a renderer.
fn harness() -> Harness {
    Fixture::new(
        "track-editor-sheet-pixels",
        TRACK_SECONDS,
        vec![
            Clip::new("pat-glow", "Glow", 2.0, 5.0).lit(),
            Clip::new("pat-glow", "Glow", 8.0, 11.0).lit(),
        ],
    )
    // Motion on, and slowed: the mid-flight frame is the point of this
    // suite, and under the suite's default snap there would be nothing
    // between "absent" and "settled" to catch.
    .with_motion()
    .with_motion_scale(4.0)
    .open(Mode::Pixel)
}

const SCRIPT: &str = r#"
    nav.trackEditor("Test Venue", "Aurora");
    nav.expand();
    nav.stageOff();
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
    app.frames(8, { waitMs: 30 });

    const sheet = () => app.snapshot().find({ role: "card", label: "Args sheet" });
    const clip = (i) => app.snapshot().findAll({ role: "card", label: "Glow" })[i];
    const waveform = () => app.snapshot().find({ role: "card", label: "Waveform" });

    const closedShot = app.screenshot();
    const closed = sheet();

    // Mid-flight: one frame after the press, before the ~270 ms slide lands.
    app.click(clip(0));
    app.frames(1);
    const midway = sheet();
    const midwayShot = app.screenshot();

    // Settled, with the schema loaded.
    until("the sheet populates", (s) =>
        s.findAll({ role: "input" }).some((n) => n.label.startsWith("intensity = ")));
    until("the slide to settle", (s) => {
        const n = s.find({ role: "card", label: "Args sheet" });
        return n !== undefined && n.bounds.width >= 319;
    });
    app.frames(4, { waitMs: 30 });
    const openBounds = sheet().bounds;
    const openShot = app.screenshot();
    const openSheet = app.screenshot({ node: sheet() });

    // Retarget to the second clip: the same box, different contents.
    app.click(clip(1));
    app.frames(10, { waitMs: 30 });
    const retargetBounds = sheet().bounds;
    const retargetSheet = app.screenshot({ node: sheet() });
    const retargetShot = app.screenshot();

    // The timeline is live under an open sheet: a press on the waveform band,
    // which the sheet does not cover, clears the selection and the sheet goes.
    app.click(waveform());
    until("the sheet to leave", (s) =>
        s.find({ role: "card", label: "Args sheet" }) === undefined);
    app.frames(6, { waitMs: 30 });
    const afterThroughClick = sheet();

    ({
      closed: closed === undefined ? null : closed.bounds,
      midway: midway === undefined ? null : midway.bounds,
      openBounds, retargetBounds,
      afterThroughClick: afterThroughClick === undefined ? null : afterThroughClick.bounds,
      closedShot, midwayShot, openShot, retargetShot, openSheet, retargetSheet,
    })
"#;

#[test]
fn the_sheet_slides_in_retargets_in_place_and_leaves_the_timeline_live() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let closed_path = support::image::keep_in("args-sheet", &out["closedShot"], "window-closed").0;
    let midway_path = support::image::keep_in("args-sheet", &out["midwayShot"], "window-midway").0;
    let open_path = support::image::keep_in("args-sheet", &out["openShot"], "window-open").0;
    let retarget_path =
        support::image::keep_in("args-sheet", &out["retargetShot"], "window-retarget").0;
    let (open_sheet_path, open_sheet) =
        support::image::keep_in("args-sheet", &out["openSheet"], "sheet-open");
    let (retarget_sheet_path, retarget_sheet) =
        support::image::keep_in("args-sheet", &out["retargetSheet"], "sheet-retargeted");
    eprintln!(
        "args sheet shots:\n  closed: {}\n  midway: {}\n  open: {}\n  retargeted: {}",
        closed_path.display(),
        midway_path.display(),
        open_path.display(),
        retarget_path.display(),
    );

    // Closed is *absent*, not a ghosted band: nothing selected, nothing drawn.
    assert!(
        out["closed"].is_null(),
        "a sheet is up with nothing selected ({})",
        closed_path.display()
    );

    // The settled sheet is the width it declares, flush against the tab's
    // right edge in a 1200×800 window.
    let (open_x, open_w) = rect(&out["openBounds"]);
    assert!(
        (open_w - 320.0).abs() < 1.0,
        "the sheet settled at {open_w}pt, not 320 ({})",
        open_path.display()
    );
    assert!(
        (open_x + open_w - 1200.0).abs() < 1.0,
        "the sheet is not flush to the window's right edge: {open_x} + {open_w} ({})",
        open_path.display()
    );

    // Mid-flight is genuinely mid-flight: some of the sheet, not all of it.
    let (mid_x, mid_w) = rect(&out["midway"]);
    assert!(
        mid_w > 0.0 && mid_w < open_w,
        "the mid-flight sheet is {mid_w}pt against a settled {open_w} ({})",
        midway_path.display()
    );
    assert!(
        (mid_x + mid_w - 1200.0).abs() < 1.0,
        "the sheet does not slide in from the right edge: {mid_x} + {mid_w} ({})",
        midway_path.display()
    );

    // Retarget is a repaint, not a reopen: the same box, different pixels.
    assert_eq!(
        out["retargetBounds"],
        out["openBounds"],
        "the sheet moved when the selection changed ({} vs {})",
        open_path.display(),
        retarget_path.display()
    );
    assert_eq!(
        (open_sheet.width(), open_sheet.height()),
        (retarget_sheet.width(), retarget_sheet.height()),
        "the two sheet shots cover different boxes"
    );

    // A click on the timeline *through* an open sheet still registers: the
    // press landed on the waveform band, cleared the selection, and the sheet
    // left. If the sheet had taken the whole region as a modal plane would,
    // nothing would have happened at all.
    assert!(
        out["afterThroughClick"].is_null(),
        "a timeline click under an open sheet did not register ({})",
        open_path.display()
    );

    // The two subjects' sheets are not byte-identical — the intensity field
    // reads 1 on one clip and whatever the other holds — but they are the
    // same chrome, so the difference is a minority of the box.
    let differing = open_sheet
        .pixels()
        .zip(retarget_sheet.pixels())
        .filter(|(a, b)| a != b)
        .count();
    let total = (open_sheet.width() * open_sheet.height()) as usize;
    assert!(
        differing * 2 < total,
        "retargeting repainted {differing} of {total} pixels — that is a \
         reopen, not a retarget\n  {} vs {}",
        open_sheet_path.display(),
        retarget_sheet_path.display(),
    );
}

/// A rect's `x` and `width`, from the node protocol.
fn rect(bounds: &Value) -> (f64, f64) {
    let read = |key: &str| {
        bounds[key]
            .as_f64()
            .unwrap_or_else(|| panic!("no {key} in {bounds:#}"))
    };
    (read("x"), read("width"))
}
