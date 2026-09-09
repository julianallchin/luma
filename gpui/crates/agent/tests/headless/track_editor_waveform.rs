//! The overview navigates the main timeline without moving playback.
#![cfg(feature = "app")]
use super::support;
use gpui_agent::Mode;
use std::time::Duration;
use support::{Clip, Fixture};

#[test]
fn minimap_pans_resizes_and_clamps_the_viewport() {
    let mut harness = Fixture::new(
        "track-editor-waveform",
        180,
        vec![Clip::new("pattern-strobe", "Strobe", 11.5, 12.5)],
    )
    .window(1480., 828.)
    .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.trackEditor("Test Venue", "Aurora");
        nav.expand();
        nav.stageOff();
        function node(label) { return app.snapshot().find({ role: "slider", label }); }
        function settle() { app.frames(12, { waitMs: 60 }); }
        settle();
        const map = node("Timeline minimap");
        const before = node("Minimap viewport").bounds;
        app.drag(node("Minimap viewport"), { dx: map.bounds.width / 4, dy: 0 });
        settle();
        const panned = node("Minimap viewport").bounds;
        app.drag(node("Minimap end"), { dx: 50, dy: 0 });
        settle();
        const resized = node("Minimap viewport").bounds;
        let viewport = node("Minimap viewport");
        app.drag(viewport, { dx: 1 - viewport.bounds.x - viewport.bounds.width / 2, dy: 0 });
        settle();
        const left = node("Minimap viewport").bounds;
        viewport = node("Minimap viewport");
        app.drag(viewport, { dx: 1479 - viewport.bounds.x - viewport.bounds.width / 2, dy: 0 });
        settle();
        const right = node("Minimap viewport").bounds;
        ({ map: map.bounds, before, panned, resized, left, right,
           modes: app.snapshot().findAll({ role: "text" }).map(n => n.label).filter(t => t.startsWith("FINE ")) })
    "#), Duration::from_secs(600));
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    let n = |name: &str, field: &str| out[name][field].as_f64().unwrap();
    assert!(n("panned", "x") > n("before", "x") + 100., "{out:#}");
    assert!(
        (n("panned", "width") - n("before", "width")).abs() < 1.,
        "{out:#}"
    );
    assert!(
        n("resized", "width") > n("panned", "width") + 30.,
        "{out:#}"
    );
    assert!((n("resized", "x") - n("panned", "x")).abs() < 1., "{out:#}");
    assert!((n("left", "x") - n("map", "x")).abs() < 1., "{out:#}");
    assert!(
        (n("right", "x") + n("right", "width") - n("map", "x") - n("map", "width")).abs() < 1.,
        "{out:#}"
    );
    assert_eq!(out["modes"].as_array().unwrap().len(), 0);
}
