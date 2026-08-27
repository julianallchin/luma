//! The args inspector strip, driven end to end.
//!
//! Four facts, each observable only through the node protocol:
//!
//! 1. **Selecting a clip populates the strip** — the timing fields read the
//!    clip's span in beats, the blend select reads its mode, and the
//!    pattern's args render as their widgets.
//! 2. **A scalar arg edit is a document write** — it survives leaving the
//!    screen and coming back, which a repaint would not.
//! 3. **A same-pattern multi-selection batch-applies** — the edit lands on
//!    the clip that was *not* under the field.
//! 4. **A mixed selection ghosts the args**, and **the strip's geometry is a
//!    constant**: empty, populated and mixed all draw the same box, and the
//!    canvas above it never moves — the no-reflow contract the ghost cells
//!    exist for.
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

/// Two clips of one pattern and one of another. At the fixture's 120 bpm a
/// beat is half a second, so Glow at 2–5 s reads 4–10 in the strip's beat
/// fields — the exactness every timing assertion below leans on.
fn harness() -> Harness {
    Fixture::new(
        "track-editor-strip",
        TRACK_SECONDS,
        vec![
            Clip::new("pat-glow", "Glow", 2.0, 5.0).lit(),
            Clip::new("pat-glow", "Glow", 8.0, 11.0).lit(),
            Clip::new("pat-wash", "Wash", 12.0, 16.0).lit().lane(1),
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
    }

    // Everything the strip says about itself: its own box, the canvas's box,
    // every input's reading, every select's value, and the text plates.
    function readStrip() {
        const shot = app.snapshot();
        const strip = shot.find({ role: "card", label: "Args strip" });
        const waveform = shot.find({ role: "card", label: "Waveform" });
        const cells = {};
        for (const node of shot.findAll({ role: "card" })) {
            if (node.label.startsWith("cell:")) cells[node.label] = node.bounds;
        }
        const inputs = {};
        for (const node of shot.findAll({ role: "input" })) {
            const at = node.label.indexOf(" = ");
            if (at > 0) inputs[node.label.slice(0, at)] = node.label.slice(at + 3);
        }
        return {
            strip: strip === undefined ? null : strip.bounds,
            waveform: waveform === undefined ? null : waveform.bounds,
            cells,
            inputs,
            selects: shot.findAll({ role: "select" }).map((n) => n.label),
            texts: shot.findAll({ role: "text" }).map((n) => n.label),
        };
    }

    function untilInput(name) {
        until("the strip shows " + name, (s) =>
            s.findAll({ role: "input" }).some((n) => n.label.startsWith(name + " = ")));
    }

    function clip(label, index) {
        return app.snapshot().findAll({ role: "card", label })[index];
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
    const empty = readStrip();

    // 1. Select the first Glow clip and wait for the schema round trip.
    app.click(clip("Glow", 0));
    untilInput("intensity");
    const populated = readStrip();

    // 2. Edit the scalar arg, then leave and come back — only a stored write
    //    survives the reopen.
    retype("intensity", "2");
    const edited = readStrip();
    nav.closeTab();
    app.frames(6);
    open();
    app.click(clip("Glow", 0));
    untilInput("intensity");
    const reopened = readStrip();

    // 3. Batch: select both Glow clips, edit, then read the *other* clip.
    app.click(clip("Glow", 1), { modifiers: ["shift"] });
    until("the batch count", (s) =>
        s.findAll({ role: "text" }).some((n) => n.label === "Glow (2)"));
    const batchStrip = readStrip();
    retype("intensity", "3");
    app.click(app.snapshot().find({ role: "card", label: "Waveform" }));
    app.frames(2);
    app.click(clip("Glow", 1));
    untilInput("intensity");
    const second = readStrip();

    // 4. Mixed: Glow + Wash. Then deselect entirely.
    app.click(clip("Wash", 0), { modifiers: ["shift"] });
    until("the mixed readout", (s) =>
        s.findAll({ role: "text" }).some((n) => n.label === "2 patterns"));
    const mixed = readStrip();
    app.click(app.snapshot().find({ role: "card", label: "Waveform" }));
    app.frames(2);
    const cleared = readStrip();

    ({ empty, populated, edited, reopened, batchStrip, second, mixed, cleared })
"#;

#[test]
fn the_strip_populates_writes_batches_and_never_moves() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // 1. Populated from the selection: the span in beats, the canonical blend
    //    name, the pattern's args as widgets.
    let populated = &out["populated"];
    assert_eq!(populated["inputs"]["start"], "4", "{populated:#}");
    assert_eq!(populated["inputs"]["end"], "10", "{populated:#}");
    assert_eq!(populated["inputs"]["intensity"], "1", "{populated:#}");
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

    // 2. The scalar edit is in the strip at once, and still there after the
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

    // 3. The batch: the same-pattern pair took the edit together, so the clip
    //    that was not under the field reads the new value alone.
    let batch_texts: Vec<String> =
        serde_json::from_value(out["batchStrip"]["texts"].clone()).expect("texts");
    assert!(
        batch_texts.iter().any(|label| label == "Glow (2)"),
        "the shared-pattern count is missing: {batch_texts:?}"
    );
    assert_eq!(
        out["second"]["inputs"]["intensity"], "3",
        "the batch write missed the second clip: {:#}",
        out["second"]
    );

    // 4. Mixed selection: the count readout, and no args to edit.
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

    // 5. The no-reflow contract: the strip's box and the canvas's box are the
    //    same in every state this test visited.
    let strip = &out["empty"]["strip"];
    let waveform = &out["empty"]["waveform"];
    assert!(
        strip.is_object(),
        "no strip in the empty state: {:#}",
        out["empty"]
    );
    for state in ["populated", "edited", "batchStrip", "mixed", "cleared"] {
        assert_eq!(
            &out[state]["strip"], strip,
            "the strip moved between empty and {state}"
        );
        assert_eq!(
            &out[state]["waveform"], waveform,
            "the timeline reflowed between empty and {state}"
        );
    }
    // And the cleared strip is the empty strip again — ghosts, no inputs.
    assert!(
        out["cleared"]["inputs"]["intensity"].is_null(),
        "deselecting did not empty the strip: {:#}",
        out["cleared"]
    );
}
