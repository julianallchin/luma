//! A press outside an open float closes it, and lands nowhere else.
//!
//! Both halves matter and they are one rule, not two. Dismissal used to be
//! wired per widget — two menus had it, the dropdowns and the colour picker
//! did not — so it now lives in the floating layer itself
//! (`luma_ui::float::Dismiss`). And because gpui hit-tests in paint order and
//! reports *every* hitbox under the pointer, a dismissal that did not take the
//! press would also act on whatever was underneath it: one gesture, two
//! effects, the second invisible until it has happened. That is the same
//! ownership rule `pointer_ownership.rs` asserts for a seam grip, one layer up.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 8;

fn harness() -> Harness {
    Fixture::new(
        "click-off",
        TRACK_SECONDS,
        vec![Clip::new("pat-glow", "Glow", 1.0, 4.0)],
    )
    .open(Mode::Headless)
}

/// The workspace's `+` menu is the case: a float hung at a window point, with
/// no dismissal of its own before this. The sidebar's track row is what sits
/// underneath the press — pressing it walks the column into that track's
/// scores, which is a loud, easy thing to notice happening by accident.
const SCRIPT: &str = r##"
    // The `+` lives in the workspace panel's band, so a tab has to be open
    // before the menu exists at all. `nav.track` leaves the sidebar back on
    // the track list, which is where the press under the menu lands.
    nav.trackEditor("Test Venue", "Aurora");

    const menu = () => app.snapshot().find({ role: "card", label: "New tab menu" });
    const level = () => app.snapshot().find({ role: "card", label: "Scores level" });

    app.action("luma::NewTab");
    const opened = until("the new-tab menu", (s) =>
        s.find({ role: "card", label: "New tab menu" }) !== undefined ? s : undefined);
    const wasListing = level() === undefined && opened !== undefined;

    // A press-and-release on the row, which is nowhere near the menu.
    // `app.click` addresses the node; the point form is only needed where
    // there is no control, and here there very much is one — that is the
    // point.
    app.click(app.snapshot().find({ role: "row", label: "Aurora" }));
    app.frames(2);

    ({ wasListing, stillOpen: menu() !== undefined, navigated: level() !== undefined })
"##;

fn run() -> Value {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

#[test]
fn a_press_outside_a_menu_closes_it_and_does_not_reach_what_is_under_it() {
    let out = run();
    assert_eq!(out["wasListing"], true, "the sidebar was not on the list");
    assert_eq!(
        out["stillOpen"], false,
        "the menu stayed up through a press outside it"
    );
    assert_eq!(
        out["navigated"], false,
        "the dismissing press also pushed the sidebar — the float is sharing its press"
    );
}
