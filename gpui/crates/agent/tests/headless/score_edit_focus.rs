//! The selected edit-focus layout and searchable dialogs, driven through input.
#![cfg(feature = "app")]
use super::support::{self, Clip, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn inspector_stays_open_and_swaps_between_presets_and_clip_inputs() {
    let mut harness = Fixture::new(
        "score-inspector-motion",
        20,
        vec![Clip::new("pat-glow", "Glow", 2., 5.).lit()],
    )
    .with_rig()
    .window(1400., 900.)
    .with_motion()
    .with_motion_scale(4.)
    .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        const node = (role,label) => app.snapshot().find({role,label});
        const read = () => {
            const shot = app.snapshot();
            const inspector = shot.find({role:"card",label:"Clip inputs"})
                ?? shot.find({role:"card",label:"Presets"});
            return {
                showing: inspector?.label ?? null,
                width: inspector?.bounds.width ?? 0,
                stage: shot.find({role:"card",label:"Stage"}).bounds,
                waveform: shot.find({role:"card",label:"Waveform"}).bounds,
            };
        };
        const sample = () => {
            const frames = [];
            for (let i = 0; i < 5; i++) {
                app.frames(1,{waitMs:20});
                frames.push(read());
            }
            return frames;
        };
        nav.trackEditor("Test Venue","Aurora");
        nav.expand();
        until("clip", () => node("card","Glow"));
        until("presets", () => node("card","Presets"));
        const empty = read();
        app.click(node("card","Glow"));
        const selecting = sample();
        until("controls", () => node("button","Pick fixtures"));
        const opened = read();
        app.click(node("card","Waveform"));
        const clearing = sample();
        until("presets again", () => node("card","Presets"));
        const cleared = read();
        ({empty,selecting,opened,clearing,cleared})
    "#,
        ),
        Duration::from_secs(60),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    assert_eq!(out["empty"]["showing"], "Presets", "{out:#}");
    assert_eq!(out["opened"]["showing"], "Clip inputs", "{out:#}");
    assert_eq!(out["cleared"]["showing"], "Presets", "{out:#}");
    let every = ["empty", "opened", "cleared"]
        .into_iter()
        .map(|state| &out[state])
        .chain(out["selecting"].as_array().unwrap())
        .chain(out["clearing"].as_array().unwrap());
    for frame in every {
        assert!(
            (frame["width"].as_f64().unwrap() - 320.).abs() < 1.,
            "the inspector never slides: {out:#}"
        );
        assert_eq!(
            frame["stage"], out["empty"]["stage"],
            "the stage keeps its space: {out:#}"
        );
        assert_eq!(
            frame["waveform"], out["empty"]["waveform"],
            "the timeline keeps its space: {out:#}"
        );
    }
}

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
        app.type(node("input","Search patterns…"), "Wash");
        app.frames(3);
        const matching = app.snapshot().findAll({role:"row"}).filter(n=>n.label==="Wash").length;
        app.key("enter");
        until("inserted", s => s.find({role:"card",label:"Constant color"}));
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
