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

/// Chip labels in the strip, which is the strip's observable content: a tab is
/// named by what it shows.
const SCRIPT: &str = r#"
    function chips() {
        return app.snapshot()
            .findAll({ role: "button" })
            .map((node) => node.label)
            .filter((label) => label === "Aurora" || label === "Zulu"
                || label === "Strobe");
    }

    nav.venue("Test Venue");
    // Zulu has no scores in this room; the sidebar lists it anyway.
    until("both tracks", (s) => s.find({ role: "row", label: "Zulu" }) !== undefined);

    nav.track("Aurora");
    until("Aurora's timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
    // This fixture runs with the stage's device off. The pane, its chrome and
    // its node must be there regardless — only the renderer is absent.
    const stageWithoutDevice =
        app.snapshot().find({ role: "card", label: "Stage" }) !== undefined;
    // A second tab in Aurora's strip, so the set being remembered is more than
    // just the editor the pick opens on its own.
    nav.pattern("Strobe");
    until("the graph", (s) => s.find({ role: "button", label: "Strobe" }) !== undefined);
    const aurora = chips();

    // Picking another track swaps the whole strip, not just the editor.
    nav.track("Zulu");
    until("Zulu's timeline", (s) => s.find({ role: "button", label: "Zulu" }) !== undefined);
    const zulu = chips();

    // …and coming back restores what Aurora had, including the pattern tab.
    nav.track("Aurora");
    until("Aurora again", (s) => s.find({ role: "button", label: "Aurora" }) !== undefined);
    const back = chips();

    ({ aurora, zulu, back, stageWithoutDevice })
"#;

#[test]
fn each_track_keeps_its_own_tabs() {
    let mut harness = harness("workspace-scope");
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let labels = |key: &str| -> Vec<String> {
        out[key]
            .as_array()
            .unwrap_or_else(|| panic!("{key} is not an array: {out:#}"))
            .iter()
            .map(|label| label.as_str().unwrap_or_default().to_string())
            .collect()
    };

    assert_eq!(
        out["stageWithoutDevice"], true,
        "the stage pane vanished when its device was switched off — the switch \
         is supposed to skip the renderer, not the pane: {out:#}"
    );

    let aurora = labels("aurora");
    assert!(
        aurora.contains(&"Aurora".to_string()) && aurora.contains(&"Strobe".to_string()),
        "Aurora's strip should hold its editor and the pattern opened beside it: {aurora:?}"
    );

    // The strip swapped rather than accumulating: Zulu inherits nothing.
    let zulu = labels("zulu");
    assert!(
        zulu.contains(&"Zulu".to_string()),
        "picking Zulu did not open its editor: {zulu:?}"
    );
    assert!(
        !zulu.contains(&"Aurora".to_string()) && !zulu.contains(&"Strobe".to_string()),
        "Aurora's tabs leaked into Zulu's strip: {zulu:?}"
    );

    // Parked is not closed.
    let back = labels("back");
    assert!(
        back.contains(&"Aurora".to_string()) && back.contains(&"Strobe".to_string()),
        "returning to Aurora did not restore its remembered tabs: {back:?}"
    );
    assert!(
        !back.contains(&"Zulu".to_string()),
        "Zulu's editor followed the eye back to Aurora: {back:?}"
    );
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
