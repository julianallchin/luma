//! The sidebar's two levels and the push between them, under a real renderer.
//!
//! Four properties, and the middle two are why the file exists:
//!
//! - the track list is the resting level, and every row is a door deeper;
//! - the push is a *push* — for real frames in the middle of it, both levels
//!   are on screen at once and the column is neither;
//! - the arriving level lists every score on the `(track, venue)`, and the one
//!   the timeline is on wears the selection ring;
//! - the pop puts the list back where it was.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test app_pixel sidebar_scores
//! ```

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 20;

/// Two further scores on the seeded `(track, venue)`, so the level has a list
/// rather than a line — and so `#2` is a row somebody can be sent to.
const EXTRA_SCORES: usize = 2;

fn harness() -> Harness {
    Fixture::new(
        "sidebar-scores-pixels",
        TRACK_SECONDS,
        vec![
            Clip::new("pat-glow", "Glow", 2.0, 5.0).lit(),
            Clip::new("pat-glow", "Glow", 8.0, 11.0).lit(),
        ],
    )
    .with_extra_scores(EXTRA_SCORES)
    .with_motion()
    // The push is 270ms. Stretched, a screenshot burst samples it several
    // times over — unstretched, a 120Hz runner and a 60Hz one disagree about
    // whether any frame lands in the middle at all.
    .with_motion_scale(10.0)
    .open(Mode::Pixel)
}

const SCRIPT: &str = r##"
    nav.venue("Test Venue");
    until("the track list", (s) => s.find({ role: "row", label: "Aurora" }) !== undefined);
    // Walk in and out of the scores once first, which is what a person does
    // before going deeper — it puts the selection ring on the row *before* the
    // baseline shot, so the pop can be compared against a picture the ring is
    // already in.
    nav.track("Aurora");
    until("the timeline", (s) =>
        s.findAll({ role: "text" }).find((n) => n.label.startsWith("SCORE #")) !== undefined);
    // Motion is on and stretched here, so the sidebar's own opening slide is
    // still running when the rows arrive. Every later assertion compares the
    // column's box against this frame's, so it has to be taken at rest.
    until("the sidebar to finish opening", (s) =>
        s.find({ role: "card", label: "Sidebar" }).bounds.width >= 255.5 ? s : undefined);
    app.frames(6, { waitMs: 16 });

    const sidebar = () => app.snapshot().find({ role: "card", label: "Sidebar" });
    const rows = () => app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));

    // One burst across a whole level change, and the mid-flight frame picked
    // out of it.
    //
    // The step is a fair fraction of the stretched push (270ms × 10), so the
    // eight frames span the flight instead of crowding one end of it — a
    // 16ms step sampled a quarter-second of a two-and-a-half-second travel
    // and every frame in it looked settled.
    //
    // The scores level is the ruler in both directions: it arrives from
    // +column-width on the push and leaves to +column-width on the pop, so
    // one positive, unclipped coordinate reads the offset either way. The
    // tracks level's own x is negative while it travels, and a node clipped
    // out of the column does not report a box you can measure with.
    // Wide, because the sampling is coarse against an eased travel: the
    // burst's step lands roughly every 500ms of a 2.7s flight and the level
    // covers most of the column in the middle of it, so a narrow band falls
    // between two frames (9px, then 163px, was a real run). Anything strictly
    // inside the column's width is part-way across, which is the whole claim —
    // a settled frame reads 0 or the full width and can never land here.
    const NEAR = 20, FAR = 220;
    const burst = () => {
        const shots = [];
        // What each frame saw, so a burst that never caught the middle says
        // where the level actually was instead of only that it missed.
        const seen = [];
        let mid = null;
        let midBounds = null;
        for (let i = 0; i < 8; i += 1) {
            const s = app.snapshot();
            const node = s.find({ role: "card", label: "Sidebar" });
            const shot = app.screenshot({ node });
            shots.push(shot);
            const x = s.find({ role: "button", label: "New score" })?.bounds.x;
            const both = s.find({ role: "input", label: "Search tracks…" }) !== undefined;
            seen.push(`${both ? "both" : "one"}@${x === undefined ? "-" : Math.round(x)}`);
            if (mid === null && both && x >= NEAR && x <= FAR) {
                mid = shot;
                midBounds = node.bounds;
            }
            app.frames(1, { waitMs: 300 });
        }
        return { shots, mid, midBounds, seen };
    };

    // The resting level, and the box every later shot is taken of. The
    // sidebar's own edge is what the two levels share, so it is the frame the
    // push has to be read in.
    const tracksBounds = sidebar().bounds;
    const tracks = app.screenshot({ node: sidebar() });

    // Into the scores. Not `nav.scores`, which waits for the push to land —
    // this test is about the frames in between.
    app.click(app.snapshot().find({ role: "row", label: "Aurora" }));
    const push = burst();
    if (push.mid === null) {
        throw new Error(`no frame of the push caught both levels part-way across: ${push.seen}`);
    }
    const midway = push.mid;
    const midwayBounds = push.midBounds;
    const flight = push.shots;

    until("the settled scores level", (s) =>
        s.find({ role: "button", label: "New score" })
            && s.find({ role: "input", label: "Search tracks…" }) === undefined ? s : undefined);
    app.frames(4, { waitMs: 16 });
    const labels = rows().map((n) => n.label);

    // Put the timeline on #2, which is the row this level exists to reach.
    app.click(rows().find((n) => n.label.startsWith("#2 ")));
    until("the timeline on #2", (s) =>
        s.findAll({ role: "text" }).find((n) => n.label === "SCORE #2") !== undefined);
    app.frames(6, { waitMs: 16 });
    const scoresBounds = sidebar().bounds;
    const scores = app.screenshot({ node: sidebar() });
    const scoresWindow = app.screenshot();

    // Back out the way we came in — and the pop is the entrance reversed, so
    // it gets the same burst and the same mid-flight test.
    app.click(app.snapshot().find({ role: "button", label: "Back to tracks" }));
    const pop = burst();
    if (pop.mid === null) {
        throw new Error(`no frame of the pop caught both levels part-way across: ${pop.seen}`);
    }
    const popMidway = pop.mid;
    const popFlight = pop.shots;

    until("the track list again", (s) =>
        s.find({ role: "input", label: "Search tracks…" })
            && s.find({ role: "card", label: "Scores level" }) === undefined ? s : undefined);
    app.frames(6, { waitMs: 16 });
    const poppedBounds = sidebar().bounds;
    const popped = app.screenshot({ node: sidebar() });

    ({ labels, tracks, midway, scores, popped, flight, popMidway, popFlight, scoresWindow,
       tracksBounds, midwayBounds, scoresBounds, poppedBounds })
"##;

#[test]
fn the_sidebar_pushes_to_a_track_s_scores_and_pops_back() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // Every score on the pair, once each, with the clips telling them apart.
    let labels: Vec<String> = out["labels"]
        .as_array()
        .expect("the level's score rows")
        .iter()
        .map(|label| label.as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        labels.len(),
        EXTRA_SCORES + 1,
        "expected every score on the pair, got {labels:?}"
    );
    let mut ordinals: Vec<&str> = labels
        .iter()
        .map(|label| label.split(' ').next().unwrap_or_default())
        .collect();
    ordinals.sort_unstable();
    assert_eq!(ordinals, ["#1", "#2", "#3"], "{labels:?}");
    assert_eq!(
        labels
            .iter()
            .filter(|label| label.contains("· 2 clips ·"))
            .count(),
        1,
        "exactly one score carries the fixture's clips: {labels:?}"
    );
    assert!(
        labels.iter().all(|label| label.contains("· You ·")),
        "a headless fixture has one principal: {labels:?}"
    );

    // The column never resizes. A level change is a change of *subject*, and
    // the regions beside it are measured from an edge that must not move.
    for other in ["midwayBounds", "scoresBounds", "poppedBounds"] {
        assert_eq!(
            out["tracksBounds"], out[other],
            "the sidebar's box moved between levels ({other})"
        );
    }

    let (tracks_path, tracks) =
        support::image::keep_in("sidebar-scores", &out["tracks"], "1-tracks");
    let (midway_path, midway) =
        support::image::keep_in("sidebar-scores", &out["midway"], "2-midway");
    let (scores_path, scores) =
        support::image::keep_in("sidebar-scores", &out["scores"], "3-scores");
    let (popped_path, popped) =
        support::image::keep_in("sidebar-scores", &out["popped"], "4-popped");
    support::image::keep_in("sidebar-scores", &out["scoresWindow"], "3-scores-window");

    let (pop_midway_path, pop_midway) =
        support::image::keep_in("sidebar-scores", &out["popMidway"], "5-pop-midway");

    let mut flight_paths = Vec::new();
    for (key, stem) in [("flight", "push"), ("popFlight", "pop")] {
        for (index, shot) in out[key]
            .as_array()
            .expect("a level-change burst")
            .iter()
            .enumerate()
        {
            let (path, _) =
                support::image::keep_in("sidebar-push", shot, &format!("{stem}-{index:02}"));
            flight_paths.push(path);
        }
    }

    eprintln!(
        "sidebar level shots:\n  tracks: {}\n  midway: {}\n  scores: {}\n  popped: {}\n  push \
         sequences: {}",
        tracks_path.display(),
        midway_path.display(),
        scores_path.display(),
        popped_path.display(),
        flight_paths
            .first()
            .map(|path| path.parent().unwrap_or(path).display().to_string())
            .unwrap_or_default(),
    );

    // The two levels are different pictures.
    let changed =
        support::image::differing_fraction(&tracks, &scores, support::image::CHANNEL_NOISE);
    // A floor on "these are different pictures", not a fraction of anything:
    // most of a 256×800 column is empty ground on both levels, and only the
    // top fifth of it carries content at all.
    assert!(
        changed > 0.05,
        "only {changed:.3} of the column changed between levels\n  {} vs {}",
        tracks_path.display(),
        scores_path.display(),
    );

    // …and the midway frame is neither of them, which is the whole claim of a
    // push: at that instant the column is showing two levels part-way across.
    for (name, settled, path) in [
        ("tracks", &tracks, &tracks_path),
        ("scores", &scores, &scores_path),
    ] {
        let apart =
            support::image::differing_fraction(&midway, settled, support::image::CHANNEL_NOISE);
        assert!(
            apart > 0.02,
            "the mid-push frame is indistinguishable from the settled {name} level \
             ({apart:.3})\n  {} vs {}",
            midway_path.display(),
            path.display(),
        );
    }

    // The pop is the entrance's reverse, so it lands back on the same picture.
    let residue =
        support::image::differing_fraction(&tracks, &popped, support::image::CHANNEL_NOISE);
    assert!(
        residue < 0.02,
        "the popped column did not settle back onto the track list ({residue:.3})\n  {} vs {}",
        tracks_path.display(),
        popped_path.display(),
    );
}
