//! The floor on the venue page.
//!
//! The floor is venue truth, like the clouds: it sits on the venue's
//! environment record, each kind of venue offers its own floors, and the
//! choice is still there when the venue page is opened again.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::Mode;
use support::Fixture;

#[test]
fn floors_follow_the_kind_of_venue_and_survive_reopening_it() {
    let mut harness = Fixture::new("venue-floor", 20, Vec::new())
        .with_rig()
        .window(1500., 1100.)
        .open(Mode::Headless);
    let script = support::script(
        r#"
        function floor() {
            const hit = app.snapshot().findAll({role:"text"})
                .map(n => n.label).find(l => l.startsWith("Floor = "));
            return hit === undefined ? null : hit.slice("Floor = ".length);
        }
        function offered() {
            return ["Grass", "Dirt", "Fine sand", "Beach", "Gravelly sand", "Asphalt",
                    "Concrete", "Gravel", "Stage deck", "Hall floor", "Black stage floor",
                    "Carpet"]
                .filter(name => app.snapshot().find({role:"toggle",label:name}) !== undefined);
        }
        // The panel is shorter than its rows: scroll to the Floor rows and
        // back to the top.
        function scrollPanel(dy) {
            app.scroll(app.snapshot().find({role:"card",label:"Venue environment"}),
                       {dy, steps: 8});
            app.frames(4);
        }
        function reopen() {
            // Let the write land, then leave the venue and come back.
            app.frames(20);
            nav.closeTab();
            nav.venuePage("Test Venue");
            until("reopened", s => s.find({role:"card",label:"Venue environment"}));
            until("saved floor", s => floor() !== null);
            return floor();
        }
        nav.patch("Test Venue");
        until("environment", s => s.find({role:"card",label:"Venue environment"}));

        nav.step("outdoor", "toggle", "Outdoor");
        until("outdoor floor", s => floor() === "Concrete");
        const outdoor = {row: floor(), offered: offered()};
        scrollPanel(-400);
        nav.step("gravel", "toggle", "Gravel");
        until("gravel chosen", s => floor() === "Gravel");

        // Moving the sun keeps the floor.
        scrollPanel(400);
        const elevation = app.snapshot().find({role:"slider",label:"Sun elevation (°)"});
        app.drag({x:elevation.bounds.x + elevation.bounds.width / 2,
                  y:elevation.bounds.y + elevation.bounds.height / 2},
                 {dx:-30, dy:0}, {steps:6});
        app.frames(4);
        const afterSun = floor();
        const outdoorReopened = reopen();

        scrollPanel(400);
        nav.step("indoor", "toggle", "Indoor");
        until("indoor floor", s => floor() === "Black stage floor");
        const indoor = {row: floor(), offered: offered()};
        scrollPanel(-400);
        nav.step("carpet", "toggle", "Carpet");
        until("carpet chosen", s => floor() === "Carpet");
        ({outdoor, afterSun, outdoorReopened, indoor, indoorReopened: reopen()})
    "#,
    );
    let result = harness.exec(&script, Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out = result.result;
    assert_eq!(out["outdoor"]["row"], "Concrete", "{out:#}");
    assert_eq!(
        out["outdoor"]["offered"],
        serde_json::json!([
            "Grass",
            "Dirt",
            "Fine sand",
            "Beach",
            "Gravelly sand",
            "Asphalt",
            "Concrete",
            "Gravel"
        ]),
        "{out:#}"
    );
    assert_eq!(out["afterSun"], "Gravel", "{out:#}");
    assert_eq!(out["outdoorReopened"], "Gravel", "{out:#}");
    assert_eq!(out["indoor"]["row"], "Black stage floor", "{out:#}");
    assert_eq!(
        out["indoor"]["offered"],
        serde_json::json!([
            "Concrete",
            "Stage deck",
            "Hall floor",
            "Black stage floor",
            "Carpet"
        ]),
        "{out:#}"
    );
    assert_eq!(out["indoorReopened"], "Carpet", "{out:#}");
}
