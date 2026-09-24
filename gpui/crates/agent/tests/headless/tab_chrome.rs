//! Outside-in contract for the workspace's new-tab menu and unified closes.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::Mode;
use serde_json::Value;
use support::{Clip, Fixture};

/// `name` is per-test: the fixture keys its seeded library directory by it,
/// and two harnesses on one name race for the same SQLite file.
fn fixture(name: &'static str) -> Fixture {
    Fixture::new(
        name,
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0)],
    )
    .with_rig()
}

/// ⌘T brings the panel back and opens the menu on it — including from a shut
/// panel, where those are one action doing two things at once.
///
/// The `+` and its menu live only in the panel now, so ⌘T has to bring the
/// panel with them or it reaches nothing at all — the regression `empty_panel`
/// records, with tabs open instead of none.
///
/// Motion is on because the interesting frames are the ones during the panel's
/// entrance: the menu is opened while the region it belongs to is still
/// arriving, and it has to be there both then and after it settles.
#[test]
fn new_tab_opens_the_panel_and_its_menu_together() {
    let mut harness = fixture("tab-chrome-new-tab")
        .with_motion()
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
            nav.trackEditor("Test Venue", "Aurora");
            until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);

            app.action("luma::ToggleWorkspace");
            until("the panel put away", (s) =>
                s.find({ role: "card", label: "Tab strip" }) === undefined);

            app.action("luma::NewTab");
            const menu = until("the new-tab menu", (s) =>
                s.find({ role: "card", label: "New tab menu" }) !== undefined ? s : undefined);
            // And it stays: a menu that survives one frame and then vanishes as
            // the panel settles is the same bug arriving late.
            app.frames(12, { waitMs: 40 });
            const settled = app.snapshot();
            ({
                opened: menu.find({ role: "button", label: "Track editor" }) !== undefined,
                stillUp: settled.find({ role: "card", label: "New tab menu" }) !== undefined,
                strip: settled.find({ role: "card", label: "Tab strip" }) !== undefined,
            })
        "#,
        ),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    assert_eq!(out["opened"], true, "⌘T opened no menu: {out:#}");
    assert_eq!(
        out["stillUp"], true,
        "the menu was dismissed while the panel it belongs to was still arriving: {out:#}"
    );
    assert_eq!(
        out["strip"], true,
        "the panel came back without its strip: {out:#}"
    );
}
