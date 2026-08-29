//! The account at the foot of the sidebar, and the menu it opens.
//!
//! A capture rather than a gate: what is being judged is whether the foot
//! reads as the end of the sidebar's column and whether the menu reads as an
//! object above it, and no number answers either. The one assertion here is
//! the thing the settings gear got wrong — a control that leaves the window
//! rather than staying reachable.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::fs;
use std::path::PathBuf;

use gpui_agent::{Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;
use support::Fixture;

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, GPU_LIVENESS_TIMEOUT);
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

fn shots_dir() -> PathBuf {
    let directory = PathBuf::from(
        std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-account-foot".into()),
    );
    fs::create_dir_all(&directory).expect("could not create the capture directory");
    directory
}

#[test]
fn capture_the_account_foot_and_its_menu() {
    let mut harness = Fixture::new("account-foot-pixels", 8, vec![])
        .window(1280.0, 800.0)
        .open(Mode::Pixel);

    let bounds = run(
        &mut harness,
        &format!(
            r#"
            {nav}
            nav.venue("Test Venue");
            nav.step("the account foot", "button", "Account");
            const opened = until("the account menu", (s) =>
                s.find({{ role: "row", label: "Settings" }}) !== undefined);
            app.frames(12, {{ waitMs: 40 }});
            const settings = opened.find({{ role: "row", label: "Settings" }});
            const foot = opened.find({{ role: "button", label: "Account" }});
            ({{
                menuTop: settings.bounds.y,
                menuLeft: settings.bounds.x,
                footTop: foot.bounds.y,
            }})
        "#,
            nav = support::NAV
        ),
    );

    // The menu opens *above* the foot and stays inside the window. Both halves
    // matter: a menu that opened downward from a bottom-docked control would
    // be snapped somewhere by the window edge and stop being attached to
    // anything, which is how the old corner gear became unreachable.
    let menu_top = bounds["menuTop"].as_f64().expect("the menu row has a top");
    let foot_top = bounds["footTop"].as_f64().expect("the foot has a top");
    assert!(
        menu_top < foot_top,
        "the account menu did not open above its foot: {bounds}"
    );
    assert!(
        bounds["menuLeft"].as_f64().expect("a left edge") >= 0.0,
        "the account menu left the window: {bounds}"
    );

    let value = run(&mut harness, "app.screenshot()");
    let source = value["path"].as_str().expect("a screenshot has a path");
    let destination = shots_dir().join("account-foot.png");
    fs::copy(source, &destination).expect("could not keep the account-foot shot");
    println!("account foot {}", destination.display());
}
