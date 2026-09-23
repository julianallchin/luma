//! Where a multi-clip vertical drag puts every clip it is holding.
//!
//! `rowToZ` (`harness/gauntlet-te/behavior-spec.md` §3.4) is a function of the
//! layer ladder *as it stood when the pointer took hold*: a selection dragged
//! up mints a new layer above the top one, which renumbers the ladder, and a
//! drag that re-read it would walk the rest of the selection a further lane on
//! every mouse move. That is only observable when two clips share a layer —
//! without the shared lane the drift lands on the same shape it should have —
//! which is why this fixture has one and the wider UX script does not.
//!
//! A separate test for the reason `track_editor_lanes` is: it needs a score of
//! its own. Its library coexists with every other fixture's in this binary.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 20;

/// Three layers over four lanes, with `Charlie` and `Cap` sharing the top one.
///
/// `Alpha` is on the floor and `Charlie` on the roof: dragging the pair up one
/// lane asks the two halves of `rowToZ` at once — one clip takes an existing
/// layer, the other mints one above the top.
const CLIPS: [(&str, f64, f64, i64); 4] = [
    ("Alpha", 2., 6., 0),
    ("Bravo", 2., 6., 1),
    ("Charlie", 2., 6., 2),
    ("Cap", 10., 14., 2),
];

/// `TRACK_HEIGHT`: one lane, which is exactly how far the drag goes.
const LANE: f64 = 80.;

fn harness() -> Harness {
    Fixture::new(
        "track-editor-stack",
        TRACK_SECONDS,
        CLIPS
            .iter()
            .map(|(name, start, end, z)| {
                Clip::new(
                    format!("pattern-{}", name.to_lowercase()),
                    *name,
                    *start,
                    *end,
                )
                .lane(*z)
            })
            .collect(),
    )
    .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    function shot() { return app.snapshot(); }
    function node(role, label) { return shot().find({ role, label }); }
    function status() { return shot().findAll({ role: "text" }).map((n) => n.label); }

    /** Which lane each clip is drawn in, by its header's top edge. Lanes, not
        pixels: the whole question is which clips ended up sharing one. */
    function stack() {
        const tops = {};
        const clips = new Set(["Alpha", "Bravo", "Charlie", "Cap"]);
        for (const card of shot().findAll({ role: "card" })) {
            if (!clips.has(card.label)) continue;
            tops[card.label] = card.bounds.y;
        }
        const order = [...new Set(Object.values(tops))].sort((a, b) => a - b);
        const out = {};
        for (const [label, y] of Object.entries(tops)) out[label] = order.indexOf(y);
        return out;
    }

    function settled() {
        // The editor shows no save state; outwait the arg debounce and the
        // write's round trip.
        app.frames(20, { waitMs: 40 });
        return true;
    }

    function open() {
        nav.track("Aurora");
        // Waited for by its result (the timeline's waveform card), not by a
        // frame count: nav.track returns as soon as the row was pressable,
        // which is earlier than the old hand-rolled walk got here, so a bare
        // frame count can land inside the load.
        until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
        nav.expand();
        // A test about the editor's own geometry, so give it the whole column:
        // the stage above it would otherwise take 40% of the height.
        nav.stageOff();
    }

    nav.venue("Test Venue");
    app.frames(8);
    open();
    const opened = stack();

    // Alpha on the floor and Charlie on the roof, dragged up together by one
    // lane off Alpha's header.
    app.click(node("card", "Alpha"));
    app.frames(2);
    app.click(node("card", "Charlie"), { modifiers: ["shift"] });
    app.frames(2);
    const selected = status();
    app.drag(node("card", "Alpha"), { dx: 0, dy: -LANE });
    app.frames(20);
    const lifted = stack();
    settled();

    nav.closeTab();
    app.frames(6);
    open();
    const stored = stack();

    ({ opened, selected, lifted, stored })
"#;

#[test]
fn a_group_dragged_up_takes_one_lane_each_however_many_moves_it_took() {
    let mut harness = harness();
    let script = SCRIPT.replace("LANE", &LANE.to_string());
    let result = harness.exec(&support::script(&script), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // 0. Three layers: Charlie and Cap share the top one, Alpha is on the
    //    floor.
    let opened = &out["opened"];
    assert_eq!(
        (rank(opened, "Charlie"), rank(opened, "Cap")),
        (0, 0),
        "the fixture did not open with two clips sharing a layer: {opened:#}"
    );
    assert_eq!(
        (rank(opened, "Bravo"), rank(opened, "Alpha")),
        (1, 2),
        "the fixture did not open on three layers: {opened:#}"
    );
    assert!(
        labels(&out["selected"]).contains(&"2 selected".to_string()),
        "shift-click did not extend the selection: {:#}",
        out["selected"]
    );

    // 1. One lane's drag is one lane each: Alpha joins the layer above it,
    //    which is Bravo's, and Charlie — already on the roof — mints a layer
    //    above everything, leaving Cap behind on what was theirs.
    //
    //    The failure this pins moved Alpha *two* lanes for a one-lane drag,
    //    because the new layer Charlie minted renumbered the ladder under the
    //    next mouse move.
    for (reading, when) in [(&out["lifted"], "on screen"), (&out["stored"], "reopened")] {
        assert_eq!(
            (rank(reading, "Charlie"), rank(reading, "Cap")),
            (0, 1),
            "{when}: {reading:#}"
        );
        let mut shared = [rank(reading, "Alpha"), rank(reading, "Bravo")];
        shared.sort_unstable();
        assert_eq!(
            shared,
            [2, 3],
            "{when}: same-priority overlaps need separate editable rows: {reading:#}"
        );
    }
    // The extra visible row must not change the layer the drag actually saved.
    let directory = support::config_dir("track-editor-stack");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool = sqlx::SqlitePool::connect(&format!(
                "sqlite:{}",
                directory.join("luma.db").display()
            ))
            .await
            .unwrap();
            // A clip names a score-local definition, and the definition's
            // name is what the timeline labelled it by.
            let layers: std::collections::BTreeMap<String, i64> =
                sqlx::query_as::<_, (String, i64)>(
                    "SELECT json_extract(definition.definition_json, '$.name'), clip.z_index
                 FROM clips clip
                 JOIN score_definitions definition
                   ON definition.id = clip.score_id || ':' || clip.graph",
                )
                .fetch_all(&pool)
                .await
                .unwrap()
                .into_iter()
                .collect();
            assert_eq!(
                (
                    layers["Alpha"],
                    layers["Bravo"],
                    layers["Charlie"],
                    layers["Cap"]
                ),
                (1, 1, 3, 2)
            );
            pool.close().await;
        });
}

/// Which lane a clip is in, counting from the topmost occupied one.
fn rank(reading: &Value, label: &str) -> u64 {
    reading[label]
        .as_u64()
        .unwrap_or_else(|| panic!("{label} is not on the timeline: {reading:#}"))
}

fn labels(reading: &Value) -> Vec<String> {
    serde_json::from_value(reading.clone()).unwrap_or_default()
}

/// Shift-selected clips copy and paste as a group, not just the last one
/// pressed: the cursor a multi-selection leaves is a point, so copy takes the
/// selection rather than the region around one clip.
#[test]
fn a_shift_selection_copies_and_pastes_every_clip() {
    let mut harness = Fixture::new(
        "track-editor-multi-copy",
        TRACK_SECONDS,
        vec![
            Clip::new("pattern-alpha", "Alpha", 1., 3.).lane(0),
            Clip::new("pattern-bravo", "Bravo", 4., 5.).lane(1),
        ],
    )
    .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        nav.venue("Test Venue");
        app.frames(8);
        nav.track("Aurora");
        until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
        nav.expand();
        nav.stageOff();
        until("the clips", (s) => s.find({ role: "card", label: "Bravo" }) !== undefined);
        const count = (label) => app.snapshot().findAll({ role: "card", label }).length;
        app.click(app.snapshot().find({ role: "card", label: "Alpha" }));
        app.frames(2);
        app.click(app.snapshot().find({ role: "card", label: "Bravo" }), { modifiers: ["shift"] });
        app.frames(2);
        app.key("ctrl-c");
        // An empty spot well after both clips: the lane's middle.
        app.click(app.snapshot().find({ role: "row", label: "Lane 0" }));
        app.frames(2);
        app.key("ctrl-v");
        app.frames(20);
        ({ alpha: count("Alpha"), bravo: count("Bravo") })
    "#,
        ),
        Duration::from_secs(120),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    assert_eq!(
        result.result,
        serde_json::json!({ "alpha": 2, "bravo": 2 }),
        "a paste dropped part of the selection"
    );
}
