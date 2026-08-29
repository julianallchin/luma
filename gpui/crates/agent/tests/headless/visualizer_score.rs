//! The stage is lit by the score the timeline is on — the *same* one.
//!
//! A `(track, venue)` pair carries as many scores as there are people who
//! annotated it, so "the score for this track here" is not a question with one
//! answer. It used to be asked twice anyway: the timeline opened the score the
//! sidebar named, and the compositor blended every score on the pair. With six
//! scores on a track that is six documents' light on the rig at once, and
//! switching between them changed nothing.
//!
//! So the claim is a correspondence rather than a value: whichever score the
//! sidebar opens, the stage names *that* one, and it keeps up when the choice
//! changes. `RIG SCORE #n` is read rather than a screenshot because the fact
//! under test is which document was installed, not what it looks like — and
//! the readout is written when the install lands, not when it is asked for.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 8;

/// Two further scores on the seeded pair, so there are three to choose
/// between and the one the fixture's clip lives on is not the only candidate.
const EXTRA_SCORES: usize = 2;

fn harness() -> Harness {
    Fixture::new(
        "visualizer-score",
        TRACK_SECONDS,
        vec![Clip::new("pat-glow", "Glow", 1.0, 4.0).lit()],
    )
    .with_extra_scores(EXTRA_SCORES)
    // The stage draws no pixels headless, but it still mounts its chrome —
    // and a rig is what makes the composite worth installing at all.
    .with_rig()
    .open(Mode::Headless)
}

const SCRIPT: &str = r##"
    nav.venue("Test Venue");
    nav.scores("Aurora");

    const rows = () =>
        app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
    // `SCORE #n` is the timeline's own readout; `RIG SCORE #n` is the stage's.
    // Disjoint prefixes, so neither find can pick the other up.
    const timeline = () =>
        app.snapshot().findAll({ role: "text" })
            .find((n) => n.label.startsWith("SCORE #"))?.label;
    const rig = () =>
        app.snapshot().findAll({ role: "text" })
            .find((n) => n.label.startsWith("RIG SCORE #"))?.label;
    const ordinal = (label) => label?.slice(label.indexOf("#"));

    // Two of the three, by the handle the sidebar names them by.
    const listed = rows().map((n) => n.label.split(" ")[0]);
    const [first, second] = listed;

    const open = (handle) => {
        const row = rows().find((n) => n.label.split(" ")[0] === handle);
        app.click(row);
        until(`the timeline on ${handle}`, () => ordinal(timeline()) === handle);
        // The install is a round trip, so the stage's readout arrives after
        // the timeline's — waiting for it is the whole point of the test.
        until(`the rig lit by ${handle}`, () => ordinal(rig()) === handle);
        return { timeline: timeline(), rig: rig() };
    };

    const one = open(first);
    // …and again, onto a score the rig is not on. A stage that had resolved
    // the score itself would sit exactly here, unmoved.
    const two = open(second);

    ({ listed, one, two })
"##;

#[test]
fn the_stage_is_lit_by_the_score_the_timeline_opened() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let listed: Vec<String> = out["listed"]
        .as_array()
        .expect("the scores level listed rows")
        .iter()
        .map(|handle| handle.as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        listed.len(),
        EXTRA_SCORES + 1,
        "expected every score on the pair, got {listed:?}"
    );

    // The correspondence, twice over: the stage names what the timeline
    // opened, and it is a *different* score the second time — an assertion
    // that would pass on any stale value if the two opens agreed.
    for (which, opened) in [("first", &out["one"]), ("second", &out["two"])] {
        let timeline = opened["timeline"].as_str().unwrap_or_default();
        let rig = opened["rig"].as_str().unwrap_or_default();
        assert_eq!(
            rig,
            &format!("RIG {timeline}"),
            "the {which} open lit the rig with something other than the open score"
        );
    }
    assert_ne!(
        out["one"]["timeline"], out["two"]["timeline"],
        "the two opens were the same score, so the switch proved nothing"
    );
}
