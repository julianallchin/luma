//! A live viewport keeps its camera across presentation and remains interactive.
#![cfg(all(feature = "app", feature = "pixel"))]
use super::support::{self, Clip, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn fullscreen_keeps_the_live_camera_and_restores_its_pose() {
    let mut harness = Fixture::new(
        "fullscreen-pixels",
        20,
        vec![Clip::new("pat-glow", "Glow", 2., 15.).lit()],
    )
    .with_rig()
    .window(1400., 900.)
    .open(Mode::Pixel);
    // GPUI's window capture exists only on macOS. Camera readings and pointer
    // input still exercise the live stage renderer on the other platforms.
    let capture = harness.exec(
        &format!("const capture = {};", cfg!(target_os = "macos")),
        Duration::from_secs(30),
    );
    assert_eq!(capture.error, None);
    let result = harness.exec(&support::script(r#"
        const node = (role,label) => app.snapshot().find({role,label});
        const screenshot = () => capture ? app.screenshot() : null;
        const camera = () => app.snapshot().findAll({role:"text"}).find(n=>n.label.startsWith("CAMERA "))?.label;
        nav.trackEditor("Test Venue","Aurora");
        until("clip", () => node("card","Glow"));
        app.click(node("card","Glow"));
        until("live stage", () => camera());
        app.frames(8,{waitMs:60});
        const before = camera();
        const embedded = screenshot();
        app.click(node("button","Fullscreen visualizer"));
        until("expanded", () => !node("card","Waveform"));
        app.frames(8,{waitMs:60});
        const expanded = camera();
        const fullscreen = screenshot();
        app.click(node("button","Exit fullscreen"));
        until("returned", () => node("card","Waveform"));
        app.frames(8,{waitMs:60});
        const returned = camera();
        const restored = screenshot();
        // Pointer focus also makes the visualizer's keyboard shortcut available.
        app.click(node("card","Stage"));
        app.key("space");
        until("play after stage click", () => node("button","Pause"));
        app.key("space");
        until("pause after stage click", () => node("button","Play"));
        app.key("shift-f");
        until("shortcut entry", () => node("button","Exit fullscreen"));
        app.drag({x:700,y:450},{dx:90,dy:40},{steps:8,restale:"match"});
        app.frames(6,{waitMs:60});
        const orbited = camera();
        app.key("escape");
        until("shortcut exit", () => node("card","Waveform"));
        app.frames(4,{waitMs:60});
        const afterOrbit = camera();
        app.action("luma::NewTab");
        nav.step("venue tab", "button", "Venue");
        until("venue controls", () => node("toggle","Stage objects"));
        app.click(node("card","Stage"));
        app.key("shift-f");
        until("venue fullscreen", () => node("button","Exit fullscreen"));
        app.key("a");
        if (node("input","Search elements")) throw new Error("hidden authoring shortcut fired");
        app.key("escape");
        until("venue restored", () => node("toggle","Stage objects"));
        app.key("a");
        until("authoring shortcut restored", () => node("input","Search elements"));
        ({before,expanded,returned,orbited,afterOrbit,embedded,fullscreen,restored})
    "#), Duration::from_secs(300));
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    for name in ["embedded", "fullscreen", "restored"] {
        if !cfg!(target_os = "macos") {
            continue;
        }
        let (path, image) = support::image::keep_in("visualizer-fullscreen", &out[name], name);
        assert_eq!(image.dimensions(), (1400, 900));
        eprintln!("{name}: {}", path.display());
    }
    assert_eq!(out["before"], out["expanded"], "entry changed the camera");
    assert_eq!(out["before"], out["returned"], "exit changed the camera");
    assert_ne!(
        out["expanded"], out["orbited"],
        "fullscreen could not orbit"
    );
    assert_eq!(out["orbited"], out["afterOrbit"], "exit lost the new pose");
}
