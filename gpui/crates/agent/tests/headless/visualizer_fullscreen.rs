//! Fullscreen is presentation of the same score, with a reversible layout.
#![cfg(feature = "app")]
use super::support::{self, Clip, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn fullscreen_restores_selection_layout_and_keeps_transport_running() {
    let mut harness = Fixture::new(
        "visualizer-fullscreen",
        60,
        vec![Clip::new("pat-glow", "Glow", 2., 50.).lit()],
    )
    .with_rig()
    .window(1400., 900.)
    .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        const node = (role,label) => app.snapshot().find({role,label});
        nav.trackEditor("Test Venue","Aurora");
        until("clip", () => node("card","Glow"));
        const emptyBar = !!node("card","Visualizer toolbar");
        app.click(node("card","Glow"));
        until("inspector", () => node("card","Clip inputs"));
        const beforeShot = app.snapshot();
        const before = Object.fromEntries(["Sidebar","Waveform","Clip inputs","Stage"].map(label => [label,beforeShot.find({role:"card",label}).bounds]));
        app.click(node("button","Play"));
        until("playing", () => node("button","Pause"));
        app.click(node("button","Fullscreen visualizer"));
        until("fullscreen", () => !node("card","Waveform"));
        const fullscreen = node("card","Fullscreen visualizer").bounds;
        const oneStage = app.snapshot().findAll({role:"card",label:"Stage"}).length;
        const hidden = ["Sidebar","Waveform","Clip inputs"].every(label => !node("card",label));
        const transport = !!node("button","Pause") && !!node("card","Visualizer toolbar");
        const time = () => app.snapshot().findAll({role:"text"}).find(n=>/^\d+:\d\d \/ \d+:\d\d$/.test(n.label))?.label;
        const firstTime = time();
        app.frames(16,{waitMs:80});
        const laterTime = time();
        app.key("space");
        until("paused", () => node("button","Play"));
        app.key("space");
        until("playing again", () => node("button","Pause"));
        // These belong to hidden editors and must not delete the selected clip,
        // close its tab, or change the remembered panel layout.
        app.key("delete ctrl-w cmd-w ctrl-b cmd-b ctrl-shift-v cmd-shift-v");
        const stillFullscreen = !!node("button","Exit fullscreen");
        app.click(node("toggle","Render settings"));
        until("settings", () => node("card","Render settings"));
        app.key("escape");
        until("settings closed", () => !node("card","Render settings"));
        const firstEscapeStayed = !!node("button","Exit fullscreen");
        app.key("escape");
        until("editor restored", () => node("card","Waveform"));
        const afterShot = app.snapshot();
        const after = Object.fromEntries(["Sidebar","Waveform","Clip inputs","Stage"].map(label => [label,afterShot.find({role:"card",label}).bounds]));
        const playing = !!node("button","Pause");
        const clipRetained = !!node("card","Glow");
        app.key("shift-f");
        until("keyboard fullscreen", () => node("button","Exit fullscreen"));
        app.click(node("button","Exit fullscreen"));
        until("button exit", () => node("card","Waveform"));
        ({emptyBar,before,after,fullscreen,oneStage,hidden,transport,firstTime,laterTime,stillFullscreen,firstEscapeStayed,playing,clipRetained})
    "#), Duration::from_secs(90));
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    assert_eq!(out["emptyBar"], false, "{out:#}");
    assert_eq!(
        out["fullscreen"],
        serde_json::json!({"x":0,"y":0,"width":1400,"height":900}),
        "{out:#}"
    );
    assert_eq!(out["oneStage"], 1, "{out:#}");
    assert_eq!(out["before"], out["after"], "{out:#}");
    assert_ne!(
        out["firstTime"], out["laterTime"],
        "playback did not advance: {out:#}"
    );
    for key in [
        "hidden",
        "transport",
        "stillFullscreen",
        "firstEscapeStayed",
        "playing",
        "clipRetained",
    ] {
        assert_eq!(out[key], true, "{key}: {out:#}");
    }
}

#[test]
fn fullscreen_venue_has_no_center_bar_and_restores_authoring_controls() {
    let mut harness = Fixture::new("venue-fullscreen", 20, Vec::new())
        .with_rig()
        .window(1200., 800.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        const node = (role,label) => app.snapshot().find({role,label});
        nav.patch("Test Venue");
        until("venue controls", () => node("toggle","Stage objects"));
        const embeddedBar = !!node("card","Visualizer toolbar");
        app.click(node("button","Fullscreen visualizer"));
        until("fullscreen", () => node("button","Exit fullscreen"));
        const noBar = !node("card","Visualizer toolbar");
        const noAuthoring = !node("toggle","Stage objects");
        app.key("a w e delete");
        const noDialog = !node("input","Search elements");
        app.key("escape");
        until("controls restored", () => node("toggle","Stage objects"));
        ({embeddedBar,noBar,noAuthoring,noDialog})
    "#,
        ),
        Duration::from_secs(60),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    for key in ["embeddedBar", "noBar", "noAuthoring", "noDialog"] {
        assert_eq!(result.result[key], true, "{}", result.result);
    }
}

#[test]
fn fullscreen_uses_intermediate_geometry_and_reverses_during_entry() {
    let mut harness = Fixture::new("fullscreen-motion", 20, Vec::new())
        .with_rig()
        .window(1400., 900.)
        .with_motion()
        .with_motion_scale(4.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        const node = (role,label) => app.snapshot().find({role,label});
        nav.trackEditor("Test Venue","Aurora");
        app.frames(40,{waitMs:50});
        const before = node("card","Stage").bounds;
        app.click(node("button","Fullscreen visualizer"));
        app.frames(3,{waitMs:25});
        const middle = node("card","Fullscreen visualizer").bounds;
        app.key("escape");
        until("reversed to editor", () => !node("card","Fullscreen visualizer"));
        const after = node("card","Stage").bounds;
        app.key("shift-f");
        until("fully expanded", () => node("card","Fullscreen visualizer")?.bounds.width >= 1399.9);
        const full = node("card","Stage").bounds;
        app.key("escape");
        until("closed", () => !node("card","Fullscreen visualizer"));
        ({before,middle,after,full})
    "#,
        ),
        Duration::from_secs(60),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    let width = |name: &str| out[name]["width"].as_f64().unwrap();
    assert!(
        width("middle") > width("before") && width("middle") < 1400.,
        "{out:#}"
    );
    assert_eq!(out["before"], out["after"], "{out:#}");
    assert!((width("full") - 1400.).abs() < 1., "{out:#}");
}
