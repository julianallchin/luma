//! The selected edit-focus layout and searchable dialogs, driven through input.
#![cfg(feature = "app")]
use super::support::{self, Clip, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn inspector_slides_with_selection_and_gives_its_space_back_to_the_stage() {
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
        const width = () => node("card","Clip inputs")?.bounds.width ?? 0;
        const read = () => {
            const shot = app.snapshot();
            return {
                width: shot.find({role:"card",label:"Clip inputs"})?.bounds.width ?? 0,
                stage: shot.find({role:"card",label:"Stage"}).bounds,
                waveform: shot.find({role:"card",label:"Waveform"}).bounds,
                controls: !!shot.find({role:"button",label:"Pick fixtures"}),
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
        until("expanded stage", () => node("card","Stage")?.bounds.width >= 1142.5);
        until("clip", () => node("card","Glow"));
        const empty = read();
        app.click(node("card","Glow"));
        const opening = sample();
        until("open inspector", () => width() >= 319.9);
        until("controls", () => node("button","Pick fixtures"));
        const opened = read();
        app.click(node("card","Waveform"));
        const closing = sample();
        until("closed inspector", () => !node("card","Clip inputs"));
        const closed = read();
        app.click(node("card","Glow"));
        until("reopened inspector", () => width() >= 319.9);
        const reopened = read();
        ({empty,opening,opened,closing,closed,reopened})
    "#,
        ),
        Duration::from_secs(60),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    assert_eq!(out["empty"]["width"], 0, "{out:#}");
    assert_eq!(out["closed"]["width"], 0, "{out:#}");
    let stage_width = out["empty"]["stage"]["width"].as_f64().unwrap();
    for phase in ["opening", "closing"] {
        let frames = out[phase].as_array().unwrap();
        assert!(
            frames.iter().any(|frame| {
                let width = frame["width"].as_f64().unwrap();
                width > 1. && width < 319.
            }),
            "the inspector snapped during {phase}: {out:#}"
        );
        let widths: Vec<_> = frames
            .iter()
            .map(|frame| frame["width"].as_f64().unwrap())
            .collect();
        assert!(
            widths.windows(2).all(|pair| if phase == "opening" {
                pair[1] >= pair[0]
            } else {
                pair[1] <= pair[0]
            }),
            "the inspector reversed during {phase}: {out:#}"
        );
        for frame in frames {
            let occupied =
                frame["width"].as_f64().unwrap() + frame["stage"]["width"].as_f64().unwrap();
            assert!((occupied - stage_width).abs() < 1., "{out:#}");
        }
    }
    assert!(
        out["closing"]
            .as_array()
            .unwrap()
            .iter()
            .all(|frame| frame["controls"] == true),
        "controls disappeared before the inspector slid away: {out:#}"
    );
    for state in ["opened", "reopened"] {
        assert!(
            (out[state]["width"].as_f64().unwrap() - 320.).abs() < 1.,
            "{out:#}"
        );
        assert_eq!(out[state]["controls"], true, "{out:#}");
    }
    assert!(
        (out["closed"]["stage"]["width"].as_f64().unwrap() - stage_width).abs() < 1.,
        "{out:#}"
    );
    assert_eq!(
        out["opened"]["waveform"], out["closed"]["waveform"],
        "{out:#}"
    );
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
