//! The fixture picker, driven from the strip it edits.
//!
//! The property pinned is the whole point of the dialog: **ticking rows is a
//! write**. An LD opens the picker off a clip's selection cell, ticks two of
//! the venue's groups, picks a rung off the "use" ladder and applies — and the
//! clip's stored selection is the union of those two groups at that subset,
//! read back through the strip's own expression field.
//!
//! The rig is on (`Fixture::with_rig`) because the picker's rows *are* the
//! venue's groups, and a venue with none has nothing to tick.
//!
//! No GPU here, so the card's left half is the renderer's failure line rather
//! than a picture. That is deliberate: the writing half of this dialog must
//! work on a machine that cannot draw the room, and a test that needed a
//! device could not say so.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

fn harness() -> Harness {
    Fixture::new(
        "fixture-picker",
        20,
        vec![Clip::new("pat-glow", "Glow", 2.0, 5.0).lit()],
    )
    .with_rig()
    .window(1400., 900.)
    .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    function expression() {
        const node = app.snapshot().findAll({ role: "input" })
            .find((n) => n.label.startsWith("expression = "));
        return node === undefined ? null : node.label.slice("expression = ".length);
    }
    function selects() {
        return app.snapshot().findAll({ role: "select" }).map((n) => n.label);
    }
    function checkbox(label) {
        return app.snapshot().find({ role: "checkbox", label });
    }

    nav.venue("Test Venue");
    app.frames(8);
    nav.track("Aurora");
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
    nav.expand();
    nav.stageOff();

    app.click(app.snapshot().find({ role: "card", label: "Glow" }));
    until("the strip", (s) =>
        s.findAll({ role: "input" }).some((n) => n.label.startsWith("expression = ")));
    const before = expression();

    // The chip beside the expression field is the LD's way in.
    app.click(app.snapshot().find({ role: "button", label: "Pick fixtures" }));
    until("the picker's rows", (s) =>
        s.find({ role: "checkbox", label: "left_movers" }) !== undefined);
    app.frames(4);
    const rows = app.snapshot().findAll({ role: "checkbox" }).map((n) => n.label);

    app.click(checkbox("left_movers"));
    app.frames(2);
    app.click(checkbox("right_movers"));
    app.frames(2);
    // The footer reads back what Apply would write, before it is written.
    const summary = app.snapshot().findAll({ role: "text" })
        .map((n) => n.label).filter((l) => l.indexOf("left_movers") >= 0);

    // The "use" ladder — the strip's rungs, in the dialog. The strip behind
    // the scrim carries a select reading "All" too, which is the point: the
    // dialog's is the one painted last.
    const ladder = () => {
        const all = app.snapshot().findAll({ role: "select", label: "All" });
        return all[all.length - 1];
    };
    app.click(ladder());
    until("the subset menu", (s) => s.find({ role: "button", label: "1/2" }) !== undefined);
    app.click(app.snapshot().find({ role: "button", label: "1/2" }));
    until("the subset to read 1/2", (s) => s.find({ role: "select", label: "1/2" }) !== undefined);

    app.click(app.snapshot().find({ role: "button", label: "Apply" }));
    until("the picker to close", (s) =>
        s.find({ role: "checkbox", label: "left_movers" }) === undefined);
    // The live edit lands at once; the write trails a 250 ms debounce.
    app.frames(8, { waitMs: 80 });
    const applied = expression();
    const appliedSubset = selects();

    // Escape leaves the clip alone: reopen, tick a third state, dismiss.
    app.click(app.snapshot().find({ role: "button", label: "Pick fixtures" }));
    until("the picker again", (s) =>
        s.find({ role: "checkbox", label: "left_movers" }) !== undefined);
    app.click(checkbox("left_movers"));
    app.frames(2);
    app.key("escape");
    until("the picker to close", (s) =>
        s.find({ role: "checkbox", label: "left_movers" }) === undefined);
    app.frames(8, { waitMs: 80 });
    const cancelled = expression();

    ({ before, rows, summary, applied, appliedSubset, cancelled })
"#;

#[test]
fn ticking_groups_writes_the_union_and_escape_writes_nothing() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // The default the fixture's pattern ships: the whole venue.
    assert_eq!(out["before"], "all", "{out:#}");
    // One row per group, and only the venue's groups.
    assert_eq!(
        out["rows"],
        serde_json::json!(["left_movers", "right_movers"]),
        "{out:#}"
    );
    // The footer says what Apply would write, in tick order.
    assert_eq!(
        out["summary"],
        serde_json::json!(["left_movers | right_movers"]),
        "{out:#}"
    );

    // The write: the union, at the rung that was picked, read back through the
    // strip's own cells rather than through the dialog that wrote it.
    assert_eq!(out["applied"], "left_movers | right_movers", "{out:#}");
    assert!(
        out["appliedSubset"]
            .as_array()
            .expect("the strip has selects")
            .iter()
            .any(|label| label == "1/2"),
        "the strip's subset cell did not follow the picker: {out:#}"
    );

    // Escape is not a quiet Apply.
    assert_eq!(out["cancelled"], "left_movers | right_movers", "{out:#}");
}
