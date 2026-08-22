//! The shell's edge regions: what ⌘B and ⌘⇧B do, and what the workspace
//! seam does.
//!
//! Three things here can silently stop working, and each one looked fine in
//! the frame the code was written in. A panel *closes* the first time and
//! never comes back, because hiding it left the keyboard with the tab it
//! stopped rendering and the next action had nowhere to land. A slide starts
//! and stalls part-way, because a manually driven tween only advances while
//! somebody asks for the next frame. A seam drags the wrong way, or by the
//! wrong amount, because the gutter arithmetic between the pointer and the
//! card's edge is off by a gap.
//!
//! Motion is snapped here (`support::Fixture::open` sets `LUMA_MOTION=off`),
//! so every reading below is a settled one: this asserts *where the regions
//! end up*, and `shell_motion` is the capture that says how they got there.

#![cfg(feature = "app")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 20;

fn harness() -> Harness {
    Fixture::new(
        "shell-panels",
        TRACK_SECONDS,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0).lane(0)],
    )
    .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    function read() {
        const shot = app.snapshot();
        const seam = shot.find({ role: "slider", label: "Workspace width" });
        const waveform = shot.find({ role: "card", label: "Waveform" });
        // By label: the composer is an input too, and it is the one that
        // answers a bare role query.
        const search = shot.find({ role: "input", label: "Search tracks…" });
        return {
            // Where the panel's left edge is, and how wide the tab body is.
            seam: seam === undefined ? null : seam.bounds.x,
            tab: waveform === undefined ? null : waveform.bounds.width,
            // The sidebar is on screen exactly when its search field is.
            sidebar: search === undefined ? null : search.bounds.width,
        };
    }

    nav.trackEditor("Test Venue", "Aurora");
    until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
    const opened = read();

    // 1. The sidebar closes and opens again. Twice, because a toggle that
    //    breaks the keyboard's home breaks on the *second* press, not the
    //    first.
    app.action("luma::ToggleSidebar");
    app.frames(2);
    const sidebarClosed = read();
    app.action("luma::ToggleSidebar");
    app.frames(2);
    const sidebarReopened = read();

    // 2. The same for the workspace panel.
    app.action("luma::ToggleWorkspace");
    app.frames(2);
    const workspaceClosed = read();
    app.action("luma::ToggleWorkspace");
    app.frames(2);
    const workspaceReopened = read();

    // 3. The seam, dragged left, widens the panel by what the pointer moved.
    //    Sidebar closed first: the panel's only ceiling is the room the
    //    thread column must keep, and this step is about the pointer, not
    //    the ceiling.
    app.action("luma::ToggleSidebar");
    app.frames(2);
    app.drag(app.snapshot().find({ role: "slider", label: "Workspace width" }), { dx: -150, dy: 0 }, { steps: 10 });
    app.frames(2);
    const widened = read();

    // 3b. Dragged absurdly far, the seam stops where the thread column's
    //     minimum begins instead of following the pointer off the edge.
    app.drag(app.snapshot().find({ role: "slider", label: "Workspace width" }), { dx: -500, dy: 0 }, { steps: 10 });
    app.frames(2);
    const maxed = read();
    app.action("luma::ToggleSidebar");
    app.frames(2);

    // 4. …and double-clicking it puts the panel back at its default width.
    app.click(app.snapshot().find({ role: "slider", label: "Workspace width" }), { count: 2 });
    app.frames(2);
    const reset = read();

    // 5. An overlay owns its pointer plane. The sidebar toggle is still in the
    //    automation tree underneath it, but pressing those coordinates must
    //    not close the covered sidebar.
    app.click(app.snapshot().find({ role: "input", label: "Search tracks…" }));
    app.action("luma::OpenPatterns");
    until("the pattern picker", (s) =>
        s.find((n) => n.role === "text" && n.label.endsWith("PATTERNS")) !== undefined);
    until("a focusable pattern row", (s) =>
        s.find({ role: "row", label: "Strobe" }) !== undefined);
    const dialog = app.snapshot().find({ role: "card", label: "Pattern dialog" });
    app.click(app.snapshot().find({ role: "button", label: "sidebar-toggle" }));
    app.frames(2);
    const overlayBlocked = read();

    // The same modal boundary applies to keyboard bindings. Escape then
    // restores the exact search field which opened the overlay.
    app.key("secondary-b");
    app.frames(2);
    const overlayKeyBlocked = read();
    app.key("tab");
    app.frames(2);
    const dialogAfterTab = app.snapshot().find({ role: "card", label: "Pattern dialog" });
    const firstAfterTab = app.snapshot().find({ role: "button", label: "Close" });
    app.key("shift-tab");
    app.frames(2);
    const dialogAfterReverse = app.snapshot().find({ role: "card", label: "Pattern dialog" });
    const firstAfterReverse = app.snapshot().find({ role: "button", label: "Close" });
    const lastAfterReverse = app.snapshot().find((n) => n.role === "row" && n.focused);
    app.key("tab");
    app.frames(2);
    const firstAfterWrap = app.snapshot().find({ role: "button", label: "Close" });
    app.key("escape");
    app.frames(2);
    const restoredSearch = app.snapshot().find({ role: "input", label: "Search tracks…" });

    ({ opened, sidebarClosed, sidebarReopened, workspaceClosed, workspaceReopened, widened, maxed, reset, overlayBlocked, overlayKeyBlocked, dialog, dialogAfterTab, firstAfterTab, dialogAfterReverse, firstAfterReverse, lastAfterReverse, firstAfterWrap, restoredSearch })
"#;

fn number(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("{key} was not a number in {value:#}"))
}

#[test]
fn the_edge_regions_toggle_both_ways_and_the_seam_resizes_the_panel() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let opened = &out["opened"];
    assert!(
        number(opened, "sidebar") > 0.0 && number(opened, "tab") > 0.0,
        "the walk did not end with both regions open: {opened:#}"
    );

    // 1. The sidebar left, and came back to where it was.
    assert!(
        out["sidebarClosed"]["sidebar"].is_null(),
        "⌘B did not close the sidebar: {:#}",
        out["sidebarClosed"]
    );
    assert!(
        number(&out["sidebarReopened"], "sidebar") > 0.0,
        "⌘B a second time did not bring the sidebar back: {:#}",
        out["sidebarReopened"]
    );

    // 2. The workspace panel, likewise — the regression that hiding it took
    //    the keyboard down with it.
    assert!(
        out["workspaceClosed"]["tab"].is_null(),
        "⌘⇧B did not close the workspace panel: {:#}",
        out["workspaceClosed"]
    );
    assert!(
        number(&out["workspaceReopened"], "tab") > 0.0,
        "⌘⇧B a second time did not bring the workspace panel back: {:#}",
        out["workspaceReopened"]
    );

    // 3. Dragging the seam 150px left moved it 150px left and gave the tab
    //    body the room — the panel grows by what the pointer travelled.
    let before = number(&out["workspaceReopened"], "tab");
    let after = number(&out["widened"], "tab");
    assert!(
        (after - before - 150.0).abs() <= 2.0,
        "the seam drag did not widen the panel by what the pointer moved: {before} -> {after}"
    );

    // 3b. Past the clamp the seam parts company with the pointer: it stops
    //     where the thread column's minimum begins (CENTER_MIN in shell.rs is
    //     360; unclamped, the same drag would park the seam near x = 0).
    assert!(
        (number(&out["maxed"], "seam") - 360.0).abs() <= 2.0,
        "the seam followed the pointer into the thread column: {:#}",
        out["maxed"]
    );

    // 4. Double-click restores the default.
    assert!(
        (number(&out["reset"], "tab") - before).abs() <= 2.0,
        "double-clicking the seam did not restore the default width: {:#}",
        out["reset"]
    );

    // 5. The modal overlay swallowed the pointer instead of toggling the
    //    sidebar hidden behind it.
    assert!(
        number(&out["overlayBlocked"], "sidebar") > 0.0,
        "the overlay let a press reach the covered sidebar toggle: {:#}",
        out["overlayBlocked"]
    );
    assert!(
        number(&out["overlayKeyBlocked"], "sidebar") > 0.0,
        "the overlay let a shell shortcut mutate the covered sidebar: {:#}",
        out["overlayKeyBlocked"]
    );
    assert_eq!(out["dialog"]["bounds"]["width"], 760.0);
    assert_eq!(out["dialog"]["bounds"]["height"], 600.0);
    assert_eq!(
        out["dialogAfterTab"]["focused"], true,
        "Tab escaped the modal focus plane: {:#}",
        out["dialogAfterTab"]
    );
    assert_eq!(
        out["firstAfterTab"]["focused"], true,
        "Tab did not move from the modal container to its first control: {:#}",
        out["firstAfterTab"]
    );
    assert_eq!(
        out["dialogAfterReverse"]["focused"], true,
        "Shift-Tab escaped the modal focus trap: {:#}",
        out["dialogAfterReverse"]
    );
    assert_eq!(
        out["firstAfterReverse"]["focused"], false,
        "Shift-Tab did not move from the first control to the last: {:#}",
        out["firstAfterReverse"]
    );
    assert_eq!(
        out["lastAfterReverse"]["focused"], true,
        "Shift-Tab did not wrap from the first control to the last row: {:#}",
        out["lastAfterReverse"]
    );
    assert_eq!(
        out["firstAfterWrap"]["focused"], true,
        "Tab did not wrap from the last dialog control to the first: {:#}",
        out["firstAfterWrap"]
    );
    assert_eq!(
        out["restoredSearch"]["focused"], true,
        "dismissing the dialog did not restore its opener: {:#}",
        out["restoredSearch"]
    );

    // The same primitive clamps rather than cropping on a compact window and
    // reserves the titlebar strip. The three custom traffic lights are still
    // present with non-empty hit targets above the modal plane.
    let mut compact = Fixture::new(
        "shell-panels-compact",
        TRACK_SECONDS,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0).lane(0)],
    )
    .window(640.0, 480.0)
    .open(Mode::Headless);
    let compact_result = compact.exec(
        &support::script(
            r#"
            nav.trackEditor("Test Venue", "Aurora");
            until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
            app.action("luma::OpenPatterns");
            until("the compact dialog", (s) => s.find({ role: "card", label: "Pattern dialog" }) !== undefined);
            const shot = app.snapshot();
            ({
                dialog: shot.find({ role: "card", label: "Pattern dialog" }),
                close: shot.find({ role: "button", label: "close" }),
                minimize: shot.find({ role: "button", label: "minimize" }),
                maximize: shot.find({ role: "button", label: "maximize" }),
            })
            "#,
        ),
        Duration::from_secs(300),
    );
    assert_eq!(
        compact_result.error, None,
        "compact script failed:\n{}",
        compact_result.stdout
    );
    let compact: Value = compact_result.result;
    assert!(number(&compact["dialog"]["bounds"], "width") <= 608.0);
    assert!(number(&compact["dialog"]["bounds"], "height") <= 426.0);
    assert!(number(&compact["dialog"]["bounds"], "y") >= 38.0);
    for control in ["close", "minimize", "maximize"] {
        assert!(
            number(&compact[control]["bounds"], "width") > 0.0,
            "{control} lost its hit target above the compact modal: {compact:#}"
        );
    }
}
