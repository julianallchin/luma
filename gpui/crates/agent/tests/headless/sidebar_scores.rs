//! The sidebar's second level, driven end to end with motion snapped.
//!
//! Four claims, none of which the column could make while a track's scores
//! lived on a strip inside the editor:
//!
//! - a track row is a door to the track's *documents*: pressing it goes a
//!   level deeper, not straight onto a timeline the sidebar guessed at;
//! - the level lists every score on the `(track, venue)`, and choosing one
//!   moves the timeline onto it without leaving the list;
//! - `New score` mints another and opens it;
//! - Back returns to the track list.
//!
//! Snapped motion (the harness default) is the point of running this here
//! rather than only under the renderer: with the push gone, a level change is
//! a swap, and every one of these gestures has to work in one frame.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 8;

/// Two further scores on the seeded pair, so the level lists three.
const EXTRA_SCORES: usize = 2;

fn harness() -> Harness {
    Fixture::new(
        "sidebar-scores",
        TRACK_SECONDS,
        vec![Clip::new("pat-glow", "Glow", 1.0, 4.0).lit()],
    )
    .with_extra_scores(EXTRA_SCORES)
    .open(Mode::Headless)
}

const SCRIPT: &str = r##"
    nav.trackEditor("Test Venue", "Aurora");
    const ordinal = () =>
        app.snapshot().findAll({ role: "text" })
            .find((n) => n.label.startsWith("SCORE #"))?.label;
    until("the timeline on some score", () => ordinal() !== undefined);
    const opened = ordinal();

    // Back in, on the row itself — `nav.trackEditor` came out to the list.
    nav.scores("Aurora");
    const rows = () =>
        app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
    const listed = rows().map((n) => n.label);
    // The track list is gone, not merely covered: a level parked off the edge
    // would still be here, and would still be a tab stop.
    const listGone = app.snapshot().find({ role: "input", label: "Search tracks…" }) === undefined;

    // Move the timeline to a score it is not on. The list stays: choosing is
    // reading, and a list that dismissed itself could not be compared.
    const other = rows().find((n) => !n.label.startsWith(opened.slice("SCORE ".length) + " "));
    app.click(other);
    until("the timeline on the other score", () => ordinal() !== opened);
    const switched = ordinal();
    const stillListing = app.snapshot().find({ role: "card", label: "Scores level" }) !== undefined;

    // Mint another, which opens it.
    app.click(app.snapshot().find({ role: "button", label: "New score" }));
    until("a fourth score, open", () => rows().length === 4 && ordinal() !== switched);
    const minted = rows().map((n) => n.label);
    const mintedOrdinal = ordinal();

    // …and back out.
    app.click(app.snapshot().find({ role: "button", label: "Back to tracks" }));
    until("the track list again", (s) =>
        s.find({ role: "input", label: "Search tracks…" })
            && s.find({ role: "card", label: "Scores level" }) === undefined ? s : undefined);
    const backOnList = app.snapshot().find({ role: "row", label: "Aurora" }) !== undefined;

    ({ opened, listed, listGone, switched, stillListing, minted, mintedOrdinal, backOnList })
"##;

#[test]
fn the_row_opens_a_track_s_scores_and_the_level_switches_mints_and_pops() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let listed: Vec<String> = strings(&out["listed"]);
    assert_eq!(
        listed.len(),
        EXTRA_SCORES + 1,
        "expected every score on the pair, got {listed:?}"
    );
    let mut listed_ordinals = ordinals(&listed);
    listed_ordinals.sort_unstable();
    assert_eq!(listed_ordinals, ["#1", "#2", "#3"], "{listed:?}");
    assert_eq!(
        listed
            .iter()
            .filter(|label| label.contains("· 1 clips ·"))
            .count(),
        1,
        "exactly one score carries the fixture's clip: {listed:?}"
    );

    assert_eq!(out["listGone"], true, "the track list stayed mounted");
    assert_ne!(
        out["opened"], out["switched"],
        "clicking another score did not move the timeline"
    );
    assert_eq!(
        out["stillListing"], true,
        "choosing a score dismissed the level it was chosen from"
    );

    // The minted score is a new one, not the idempotent hand-back of the
    // score already on the pair — which is the whole difference between this
    // row and Add-to-venue.
    let minted: Vec<String> = strings(&out["minted"]);
    let mut minted_ordinals = ordinals(&minted);
    minted_ordinals.sort_unstable();
    assert_eq!(minted_ordinals, ["#1", "#2", "#3", "#4"], "{minted:?}");
    assert_eq!(
        out["mintedOrdinal"], "SCORE #4",
        "the minted score is not the one on the timeline: {minted:?}"
    );
    assert_eq!(
        out["backOnList"], true,
        "Back did not restore the track list"
    );
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("an array of labels")
        .iter()
        .map(|label| label.as_str().unwrap_or_default().to_string())
        .collect()
}

fn ordinals(labels: &[String]) -> Vec<&str> {
    labels
        .iter()
        .map(|label| label.split(' ').next().unwrap_or_default())
        .collect()
}
