//! The args sheet, driven end to end.
//!
//! Six facts, each observable only through the node protocol:
//!
//! 1. **Selecting a clip brings the sheet in** — it is absent with nothing
//!    selected, present with a clip selected, and reads that clip's blend
//!    mode and args. The clip's span is deliberately absent: bounds are
//!    edited on the timeline.
//! 2. **Every arg is reachable.** The strip this replaced ran a pattern's
//!    third arg off the right edge of a 1200pt window; a column cannot, so
//!    every row the schema declares is inside the sheet's own box.
//! 3. **A scalar arg edit is a document write** — it survives leaving the
//!    screen and coming back, which a repaint would not.
//! 4. **A same-pattern multi-selection batch-applies** — the edit lands on
//!    the clip that was *not* under the field.
//! 5. **Retarget, not reopen**: clicking a second clip while the sheet is up
//!    leaves the sheet's own box exactly where it was and swaps its contents.
//!    And the timeline stays live underneath — a click on a lane the sheet
//!    does not cover still registers.
//! 6. **A mixed selection offers no args**, and clearing the selection sends
//!    the sheet away.
//!
//! The clips are lit (`Clip::lit`) because arg definitions live on a
//! pattern's *graph document*, and only the lit path authors one; two clips
//! share one pattern key, which the fixture now mints once.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 20;

/// Two clips of one pattern and one of another.
fn harness() -> Harness {
    Fixture::new(
        "track-editor-sheet",
        TRACK_SECONDS,
        vec![
            Clip::new("pat-glow", "Glow", 2.0, 5.0).lit(),
            Clip::new("pat-glow", "Glow", 8.0, 11.0).lit(),
            // Early on the timeline on purpose: the sheet overlays the
            // right of the canvas, and a clip a test has to click cannot
            // live under it.
            Clip::new("pat-wash", "Wash", 4.0, 7.0).lit().lane(1),
        ],
    )
    .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    function open() {
        nav.track("Aurora");
        until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
        nav.expand();
        nav.stageOff();
        // The waveform lands before the score does; the clips are what this
        // test clicks, so wait for them rather than for the picture.
        until("the clips", (s) =>
            s.findAll({ role: "card", label: "Glow" }).length === 2);
    }

    // Everything the sheet says about itself: its own box, the canvas's box,
    // every input's reading, every select's value, the labelled rows and the
    // text plates.
    function readSheet() {
        const shot = app.snapshot();
        const sheet = shot.find({ role: "card", label: "Args sheet" });
        const waveform = shot.find({ role: "card", label: "Waveform" });
        const inputs = {};
        for (const node of shot.findAll({ role: "input" })) {
            const at = node.label.indexOf(" = ");
            if (at > 0) inputs[node.label.slice(0, at)] = node.label.slice(at + 3);
        }
        const rows = {};
        for (const node of shot.findAll({ role: "row" })) rows[node.label] = node.bounds;
        return {
            sheet: sheet === undefined ? null : sheet.bounds,
            waveform: waveform === undefined ? null : waveform.bounds,
            inputs,
            rows,
            selects: shot.findAll({ role: "select" }).map((n) => n.label),
            texts: shot.findAll({ role: "text" }).map((n) => n.label),
        };
    }

    function untilInput(name) {
        until("the sheet shows " + name, (s) =>
            s.findAll({ role: "input" }).some((n) => n.label.startsWith(name + " = ")));
    }
    function untilGone() {
        until("the sheet to leave", (s) =>
            s.find({ role: "card", label: "Args sheet" }) === undefined);
    }

    function clip(label, index) {
        return app.snapshot().findAll({ role: "card", label })[index];
    }
    function waveform() {
        return app.snapshot().find({ role: "card", label: "Waveform" });
    }

    // Focus a drafted field, replace its content, commit. Characters go
    // through `app.type` — the harness's text-insertion path — because a bare
    // digit keystroke is not text input; the field would never hear it.
    function field(name) {
        return app.snapshot().findAll({ role: "input" })
            .find((n) => n.label.startsWith(name + " = "));
    }
    function retype(name, digits) {
        app.click(field(name));
        app.key("cmd-a backspace");
        app.type(field(name), digits, { restale: "match" });
        app.key("enter");
        // The live edit lands at once; the write trails a 250 ms debounce and
        // a round trip. Give it both.
        app.frames(8, { waitMs: 80 });
    }

    nav.venue("Test Venue");
    app.frames(8);
    open();
    const empty = readSheet();

    // 1. Select the first Glow clip and wait for the schema round trip.
    app.click(clip("Glow", 0));
    untilInput("intensity");
    app.frames(12, { waitMs: 30 });
    const populated = readSheet();

    // 2. Edit the scalar arg, then leave and come back — only a stored write
    //    survives the reopen.
    retype("intensity", "2");
    const edited = readSheet();
    nav.closeTab();
    app.frames(6);
    open();
    app.click(clip("Glow", 0));
    untilInput("intensity");
    app.frames(12, { waitMs: 30 });
    const reopened = readSheet();

    // 3. Retarget: click the OTHER Glow clip with the sheet already up. No
    //    close, no reopen — the same box, a different subject.
    app.click(clip("Glow", 1));
    app.frames(6, { waitMs: 30 });
    const retargeted = readSheet();

    // 4. The timeline is still live under an open sheet: a press on the
    //    waveform band (which the sheet does not cover) clears the selection.
    app.click(waveform());
    untilGone();
    const throughClick = readSheet();

    // 5. Batch: select both Glow clips, edit, then read the *other* clip.
    app.click(clip("Glow", 0));
    untilInput("intensity");
    app.click(clip("Glow", 1), { modifiers: ["shift"] });
    until("the batch count", (s) =>
        s.findAll({ role: "text" }).some((n) => n.label === "Glow (2)"));
    retype("intensity", "3");
    app.click(waveform());
    untilGone();
    app.click(clip("Glow", 1));
    untilInput("intensity");
    const second = readSheet();

    // 6. Mixed: Glow + Wash. Then Escape, which clears the selection.
    app.click(clip("Wash", 0), { modifiers: ["shift"] });
    until("the mixed readout", (s) =>
        s.findAll({ role: "text" }).some((n) => n.label === "2 patterns"));
    const mixed = readSheet();
    app.key("escape");
    untilGone();
    const cleared = readSheet();

    ({ empty, populated, edited, reopened, retargeted, throughClick, second, mixed, cleared })
"#;

#[test]
fn the_sheet_arrives_writes_batches_retargets_and_leaves() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // 1. Nothing selected is nothing drawn: the sheet is not a permanent
    //    plane, which is the whole difference from the strip it replaced.
    assert!(
        out["empty"]["sheet"].is_null(),
        "the sheet is up with nothing selected: {:#}",
        out["empty"]
    );

    // Populated from the selection: the canonical blend name and the
    // pattern's args as widgets — and no span, which the timeline owns.
    let populated = &out["populated"];
    assert!(populated["sheet"].is_object(), "{populated:#}");
    assert_eq!(populated["inputs"]["intensity"], "1", "{populated:#}");
    for bound in ["start", "end"] {
        assert!(
            populated["inputs"][bound].is_null(),
            "the sheet still offers a {bound} field: {populated:#}"
        );
    }
    assert!(
        populated["inputs"]["expression"]
            .as_str()
            .is_some_and(|reading| reading.contains("all")),
        "the selection arg did not seed its expression: {populated:#}"
    );
    let selects: Vec<String> =
        serde_json::from_value(populated["selects"].clone()).expect("selects");
    assert!(
        selects.iter().any(|label| label == "replace"),
        "the blend select does not read the clip's mode: {selects:?}"
    );
    let texts: Vec<String> = serde_json::from_value(populated["texts"].clone()).expect("texts");
    assert!(
        texts.iter().any(|label| label == "Glow"),
        "the pattern readout is missing: {texts:?}"
    );

    // 2. Every row the fixture's schema declares is *inside* the sheet, which
    //    is what the horizontal strip could not promise: its third arg (the
    //    colour) fell off the right edge of the same 1200pt window.
    let sheet = rect(&populated["sheet"]);
    let rows = populated["rows"].as_object().expect("rows");
    for name in ["blend", "selection", "how many", "intensity", "tint"] {
        let row = rows
            .get(name)
            .unwrap_or_else(|| panic!("no `{name}` row: {populated:#}"));
        let row = rect(row);
        assert!(
            row.0 >= sheet.0 - 1.0 && row.1 <= sheet.1 + 1.0,
            "the `{name}` row runs outside the sheet: {row:?} vs {sheet:?}"
        );
    }

    // 3. The scalar edit is in the sheet at once, and still there after the
    //    screen was torn down and reopened — a stored write, not a repaint.
    assert_eq!(
        out["edited"]["inputs"]["intensity"], "2",
        "{:#}",
        out["edited"]
    );
    assert_eq!(
        out["reopened"]["inputs"]["intensity"], "2",
        "the arg edit did not survive a reopen: {:#}",
        out["reopened"]
    );

    // 4. Retarget in place: the sheet's own box is identical across a change
    //    of subject — no exit, no entrance, no bounce.
    assert_eq!(
        out["retargeted"]["sheet"], out["reopened"]["sheet"],
        "the sheet moved when the selection changed: {:#}",
        out["retargeted"]
    );
    assert_eq!(
        out["retargeted"]["inputs"]["intensity"], "1",
        "the sheet did not retarget to the second clip: {:#}",
        out["retargeted"]
    );

    // 5. A click on the timeline *through* an open sheet still registers: the
    //    press on the waveform band cleared the selection, so the sheet left.
    assert!(
        out["throughClick"]["sheet"].is_null(),
        "a timeline click under an open sheet did not register: {:#}",
        out["throughClick"]
    );

    // 6. The batch: the same-pattern pair took the edit together, so the clip
    //    that was not under the field reads the new value alone.
    assert_eq!(
        out["second"]["inputs"]["intensity"], "3",
        "the batch write missed the second clip: {:#}",
        out["second"]
    );

    // 7. Mixed selection: the count readout, and no args to edit.
    let mixed = &out["mixed"];
    let mixed_texts: Vec<String> = serde_json::from_value(mixed["texts"].clone()).expect("texts");
    assert!(
        mixed_texts.iter().any(|label| label == "2 patterns"),
        "{mixed_texts:?}"
    );
    assert!(
        mixed_texts.iter().any(|label| label == "Mixed patterns"),
        "{mixed_texts:?}"
    );
    assert!(
        mixed["inputs"]["intensity"].is_null(),
        "a mixed selection must not offer args: {mixed:#}"
    );

    // 8. Escape cleared the selection, and the sheet went with it.
    assert!(
        out["cleared"]["sheet"].is_null(),
        "escape did not send the sheet away: {:#}",
        out["cleared"]
    );

    // The timeline never reflows for the sheet: it overlays, it does not
    // occupy, so the canvas's box is the same in every state.
    let waveform = &out["empty"]["waveform"];
    for state in ["populated", "edited", "retargeted", "mixed", "cleared"] {
        assert_eq!(
            &out[state]["waveform"], waveform,
            "the timeline reflowed between empty and {state}"
        );
    }
}

/// A rect's left and right edges, from the node protocol's `{ x, width }`.
fn rect(bounds: &Value) -> (f64, f64) {
    let read = |key: &str| {
        bounds[key]
            .as_f64()
            .unwrap_or_else(|| panic!("no {key} in {bounds:#}"))
    };
    (read("x"), read("x") + read("width"))
}
