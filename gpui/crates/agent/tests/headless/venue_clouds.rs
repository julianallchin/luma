//! Sky clouds on the venue page.
//!
//! The cloud preset is venue truth, like the sun: it sits on the venue's
//! environment record, so it is only offered where there is a sky, and it is
//! still there when the venue page is opened again.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::Mode;
use support::Fixture;

#[test]
fn cloud_presets_are_outdoor_only_and_survive_reopening_the_venue() {
    let mut harness = Fixture::new("venue-clouds", 20, Vec::new())
        .with_rig()
        .window(1500., 950.)
        .open(Mode::Headless);
    let script = support::script(
        r#"
        function clouds() {
            const hit = app.snapshot().findAll({role:"text"})
                .map(n => n.label).find(l => l.startsWith("Clouds = "));
            return hit === undefined ? null : hit.slice("Clouds = ".length);
        }
        function offered() {
            return ["Clear", "Fair weather", "Scattered", "Overcast", "Storm"]
                .filter(name => app.snapshot().find({role:"toggle",label:name}) !== undefined);
        }
        nav.patch("Test Venue");
        until("environment", s => s.find({role:"card",label:"Venue environment"}));
        nav.step("indoor", "toggle", "Indoor");
        app.frames(4);
        const indoor = {row: clouds(), offered: offered()};

        nav.step("outdoor", "toggle", "Outdoor");
        until("cloud row", s => clouds() !== null);
        const outdoor = {row: clouds(), offered: offered()};
        nav.step("overcast", "toggle", "Overcast");
        until("overcast chosen", s => clouds() === "Overcast");

        // Moving the sun keeps the weather.
        const elevation = app.snapshot().find({role:"slider",label:"Sun elevation (°)"});
        app.drag({x:elevation.bounds.x + elevation.bounds.width / 2,
                  y:elevation.bounds.y + elevation.bounds.height / 2},
                 {dx:-30, dy:0}, {steps:6});
        app.frames(4);
        const afterSun = clouds();

        // Let the write land, then leave the venue and come back.
        app.frames(20);
        nav.closeTab();
        nav.venuePage("Test Venue");
        until("reopened", s => s.find({role:"card",label:"Venue environment"}));
        until("saved clouds", s => clouds() !== null);
        ({indoor, outdoor, afterSun, reopened: clouds()})
    "#,
    );
    let result = harness.exec(&script, Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out = result.result;
    assert_eq!(out["indoor"]["row"], serde_json::Value::Null, "{out:#}");
    assert_eq!(out["indoor"]["offered"], serde_json::json!([]), "{out:#}");
    assert_eq!(out["outdoor"]["row"], "Clear", "{out:#}");
    assert_eq!(
        out["outdoor"]["offered"],
        serde_json::json!(["Clear", "Fair weather", "Scattered", "Overcast", "Storm"]),
        "{out:#}"
    );
    assert_eq!(out["afterSun"], "Overcast", "{out:#}");
    assert_eq!(out["reopened"], "Overcast", "{out:#}");
}
