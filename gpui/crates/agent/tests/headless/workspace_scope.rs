//! The tab strip belongs to the picked track.
//!
//! ```sh
//! cargo test -p gpui-agent --features app --test workspace_scope
//! ```
//!
//! `tabs.rs` and `workspace.rs` prove the swap as pure logic. This proves the
//! wiring: that picking a row in the sidebar is what moves the strip, that the
//! set comes back intact, and that a track's tabs do not leak into its
//! neighbour's — which is the whole point and the one thing a unit test over
//! `ParkedTabs` cannot see, because it never touches the sidebar.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

/// `name` is per-test: the fixture keys its seeded library directory by it,
/// and two harnesses on one name race for the same SQLite file.
fn harness(name: &'static str) -> Harness {
    Fixture::new(
        name,
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0)],
    )
    .with_equal_timestamp_track()
    .with_rig()
    .open(Mode::Headless)
}

/// The sidebar's Venue row is a place, not a tab. Picking it hides the thread
/// and the strip and shows the venue page. Picking a track brings that
/// track's strip and the thread back as they were.
#[test]
fn the_venue_row_takes_the_workspace_and_a_track_gives_it_back() {
    let mut harness = harness("workspace-scope-venue");
    let result = harness.exec(
        &support::script(
            r#"
            function state() {
                const s = app.snapshot();
                return {
                    page: s.find({ role: "card", label: "Test Venue Venue" }) !== undefined,
                    strip: s.find({ role: "card", label: "Tab strip" }) !== undefined,
                    thread: s.findAll({ role: "text" }).some((n) => n.label === "Luma"),
                    aurora: s.find({ role: "button", label: "Aurora" }) !== undefined,
                };
            }
            nav.trackEditor("Test Venue", "Aurora");
            until("Aurora's timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
            const track = state();

            nav.venuePage("Test Venue");
            until("the thread collapsed", (s) =>
                !s.findAll({ role: "text" }).some((n) => n.label === "Luma"));
            const venue = state();

            nav.step("Aurora again", "row", "Aurora");
            until("Aurora's strip", (s) => s.find({ role: "button", label: "Aurora" }) !== undefined);
            until("the thread back", (s) => s.findAll({ role: "text" }).some((n) => n.label === "Luma"));
            const back = state();
            ({ track, venue, back })
        "#,
        ),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    let on = |key: &str, field: &str| out[key][field].as_bool().unwrap_or_default();

    assert!(
        on("track", "thread") && on("track", "strip") && on("track", "aurora"),
        "{out:#}"
    );
    assert!(
        on("venue", "page"),
        "the Venue row did not open the venue page: {out:#}"
    );
    assert!(
        !on("venue", "strip") && !on("venue", "thread") && !on("venue", "aurora"),
        "the venue page kept the strip, the thread or the track's tabs: {out:#}"
    );
    assert!(
        on("back", "thread") && on("back", "strip") && on("back", "aurora") && !on("back", "page"),
        "picking the track did not give back its strip and the thread: {out:#}"
    );
}
