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

fn harness() -> Harness {
    Fixture::new(
        "workspace-scope",
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
                || label === "Strobe" || label === "Test Venue Patch");
    }

    nav.venue("Test Venue");
    // Zulu has no scores in this room, and the sidebar opens filtered to the
    // ones that do. This test is about two tracks, so widen it to the library.
    nav.step("the in-venue filter", "toggle", "In Venue");
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
    let mut harness = harness();
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
