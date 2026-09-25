//! The reflection probe switches in the render settings' Debug section.
//!
//! Both are session view choices: "Reflection probes" turns the local probes
//! on and off, "Show probes" draws them as balls. Each click must change
//! what the renderer is handed, which the section reports as
//! `Renderer probes = on|off, balls on|off`.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::Mode;
use support::Fixture;

#[test]
fn the_probe_switches_change_what_the_renderer_draws() {
    let mut harness = Fixture::new("probe-view", 20, Vec::new())
        .with_rig()
        .window(1500., 1100.)
        .open(Mode::Headless);
    let script = support::script(
        r#"
        function toggled(name) {
            const node = app.snapshot().find({role:"toggle",label:name});
            return node === undefined ? null : node.focused;
        }
        function sent() {
            const hit = app.snapshot().findAll({role:"text"})
                .map(n => n.label).find(l => l.startsWith("Renderer probes = "));
            return hit === undefined ? null : hit.slice("Renderer probes = ".length);
        }
        function state() {
            return {probes: toggled("Reflection probes"), balls: toggled("Show probes"),
                    sent: sent()};
        }
        nav.patch("Test Venue");
        nav.step("view settings", "toggle", "Render settings");
        until("the debug switches", s =>
            s.find({role:"toggle",label:"Show probes"}) !== undefined ? s : undefined);
        const before = state();
        nav.step("show probes", "toggle", "Show probes");
        until("balls on", s => sent() === "on, balls on");
        const balls = state();
        nav.step("probes off", "toggle", "Reflection probes");
        until("probes off", s => sent() === "off, balls on");
        const off = state();
        ({before, balls, off})
    "#,
    );
    let result = harness.exec(&script, Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out = result.result;
    // The start-up defaults: probes on, balls off (no LUMA_PROBES or
    // LUMA_PROBE_DEBUG in the test's environment).
    assert_eq!(out["before"]["probes"], true, "{out:#}");
    assert_eq!(out["before"]["balls"], false, "{out:#}");
    assert_eq!(out["before"]["sent"], "on, balls off", "{out:#}");
    assert_eq!(out["balls"]["balls"], true, "{out:#}");
    assert_eq!(out["balls"]["sent"], "on, balls on", "{out:#}");
    assert_eq!(out["off"]["probes"], false, "{out:#}");
    assert_eq!(out["off"]["sent"], "off, balls on", "{out:#}");
}
