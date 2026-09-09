//! The selected edit-focus layout and searchable dialogs, driven through input.
#![cfg(feature = "app")]
use super::support::{self, Clip, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn inspector_stays_above_timeline_and_pickers_accept_keyboard_input() {
    let mut harness = Fixture::new(
        "score-edit-focus",
        20,
        vec![Clip::new("pat-glow", "Glow", 2., 5.).lit()],
    )
    .with_rig()
    .window(1400., 900.)
    .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        const node = (role,label) => app.snapshot().find({role,label});
        nav.venue("Test Venue");
        nav.track("Aurora");
        nav.expand();
        until("clip", () => node("card","Glow"));
        app.click(node("card","Glow"));
        until("controls", () => node("button","Pick fixtures"));
        const before = node("card","Clip inputs").bounds;
        app.drag(node("slider","Stage height"),{dx:0,dy:140});
        app.frames(4);
        const inspector = node("card","Clip inputs").bounds;
        const wave = node("card","Waveform").bounds;
        const controls = node("button","Pick fixtures").bounds;
        app.click(node("button","Pick fixtures"));
        until("selection search", () => node("input","Search groups…"));
        app.scroll(node("checkbox","left_movers"), {dy:0});
        app.frames(2);
        const left = node("checkbox","left_movers").bounds;
        app.scroll({x:left.x+left.width/2, y:left.y+left.height+1}, {dy:0});
        const held = !!node("text","Preview: left_movers");
        app.type(node("input","Search groups…"), "right");
        app.frames(3);
        const filtered = app.snapshot().findAll({role:"checkbox"}).map(n=>n.label);
        app.key("down enter");
        app.click(node("button","Apply"));
        until("selection applied", s => s.findAll({role:"input"}).some(n=>n.label==="expression = right_movers"));
        app.click(node("row","Lane 0"),{button:"right"});
        until("pattern dialog", () => node("card","Insert pattern dialog"));
        const dialog = node("card","Insert pattern dialog").bounds;
        app.type(node("input","Search patterns…"), "Glow");
        app.frames(3);
        const matching = app.snapshot().findAll({role:"row"}).filter(n=>n.label==="Glow").length;
        app.key("enter");
        until("inserted", s => s.findAll({role:"card",label:"Glow"}).length===2);
        const closed = !node("card","Insert pattern dialog");
        ({before, inspector, wave, controls, filtered, dialog, matching, closed, held})
    "#), Duration::from_secs(300));
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    let n = |part: &str, key: &str| out[part][key].as_f64().unwrap();
    assert!(n("inspector", "height") >= n("before", "height"), "{out:#}");
    assert!(n("inspector", "height") >= 300., "{out:#}");
    assert!(
        n("inspector", "y") + n("inspector", "height") <= n("wave", "y"),
        "{out:#}"
    );
    assert!(n("controls", "height") > 15., "{out:#}");
    assert!(n("dialog", "height") > 400., "{out:#}");
    assert_eq!(
        out["filtered"],
        serde_json::json!(["right_movers"]),
        "{out:#}"
    );
    assert_eq!(out["matching"], 1, "{out:#}");
    assert_eq!(out["held"], true, "{out:#}");
    assert_eq!(out["closed"], true, "{out:#}");
}
