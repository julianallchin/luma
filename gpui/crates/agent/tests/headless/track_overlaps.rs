#![cfg(feature = "app")]
use super::support;
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn agent_score_overlaps_have_separate_clickable_rows_and_independent_edits() {
    let mut harness = support::Fixture::new("track-overlaps", 20, vec![])
        // Three clips of one definition, deliberately: what is under test is
        // that identical spans get separate rows and separate hit targets.
        .with_graph_score(support::score(
            serde_json::json!({ "overlap": support::definition("Chase") }),
            serde_json::json!({
                "a":{"graph":"overlap","start":2.,"duration":6.,"seed":0,"z_index":0},
                "b":{"graph":"overlap","start":2.,"duration":6.,"seed":1,"z_index":0},
                "c":{"graph":"overlap","start":2.,"duration":6.,"seed":2,"z_index":0}
            }),
        ))
        .window(1400., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        nav.trackEditor("Test Venue","Aurora");
        nav.expand();
        nav.stageOff();
        const clips = () => app.snapshot().findAll({role:"card",label:"Chase"})
            .sort((a,b) => a.bounds.y-b.bounds.y);
        until("three overlapping clips", () => clips().length === 3);
        app.action("luma::FitLanes");
        const initial = clips().map(n => n.bounds);
        for (let i=1;i<initial.length;i++) {
            if (initial[i].y < initial[i-1].y + initial[i-1].height)
                throw new Error("overlapping clip hit targets");
        }
        for (let i=0;i<3;i++) {
            app.click(clips()[i]);
            until("individual inspector", s => !!s.find({role:"card",label:"Clip inputs"}));
        }
        // Removing the middle visible clip leaves both other clips intact.
        app.click(clips()[1]);
        app.key("backspace");
        until("only the selected clip removed", () => clips().length === 2);
        app.frames(10,{waitMs:50});
        nav.closeTab();
        nav.track("Aurora");
        until("edit persisted", () => clips().length === 2);
        ({initial,remaining:clips().map(n => n.bounds)})
    "#,
        ),
        Duration::from_secs(120),
    );
    assert_eq!(result.error, None, "{}\n{:#}", result.stdout, result.result);
}
