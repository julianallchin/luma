//! Edit-focus layout and both picker previews under the native renderer.
#![cfg(all(feature = "app", feature = "pixel"))]
use super::support::{self, Clip, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn edit_focus_keeps_controls_usable_and_renders_both_picker_previews() {
    let mut harness = Fixture::new(
        "edit-focus-pixels",
        20,
        vec![Clip::new("pat-glow", "Glow", 2., 5.).lit()],
    )
    .with_rig()
    .window(1400., 900.)
    .open(Mode::Pixel);
    let result = harness.exec(
        &support::script(
            r#"
        const node = (role,label) => app.snapshot().find({role,label});
        nav.trackEditor("Test Venue","Aurora");
        nav.expand();
        until("clip", () => node("card","Glow"));
        app.click(node("card","Glow"));
        until("controls", () => node("button","Pick fixtures"));
        app.frames(10,{waitMs:50});
        const normal = app.screenshot();
        app.drag(node("slider","Stage height"),{dx:0,dy:160});
        app.frames(5,{waitMs:50});
        const compact = app.screenshot();
        const inspector = node("card","Clip inputs").bounds;
        const timeline = node("card","Waveform").bounds;
        app.click(node("button","Pick fixtures"));
        until("selection preview", () => node("card","Selection preview"));
        app.frames(8,{waitMs:50});
        const selection = app.screenshot();
        app.click(node("checkbox","left_movers"));
        app.frames(8,{waitMs:50});
        const highlighted = app.screenshot();
        app.key("escape");
        until("picker closed", () => !node("card","Fixture picker dialog"));
        app.click(node("button","Add pattern"));
        until("pattern search", () => node("input","Search patterns…"));
        app.type(node("input","Search patterns…"),"Glow");
        until("pattern preview", () => node("card","Pattern preview"));
        app.frames(8,{waitMs:50});
        const pattern = app.screenshot();
        ({normal,compact,selection,highlighted,pattern,inspector,timeline})
    "#,
        ),
        Duration::from_secs(600),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    for name in ["normal", "compact", "selection", "highlighted", "pattern"] {
        let (path, image) = support::image::keep_in("score-edit-focus", &out[name], name);
        assert_eq!((image.width(), image.height()), (1400, 900));
        eprintln!("{name}: {}", path.display());
    }
    let inspector = &out["inspector"];
    assert!(inspector["height"].as_f64().unwrap() >= 300.);
    assert!(
        inspector["y"].as_f64().unwrap() + inspector["height"].as_f64().unwrap()
            <= out["timeline"]["y"].as_f64().unwrap()
    );
}
