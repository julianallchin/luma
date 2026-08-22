//! Shoot the shell's panel slides, frame by frame.
//!
//! ```sh
//! LUMA_MOTION=on LUMA_MOTION_SCALE=10 \
//!   cargo test --release -p gpui-agent --features pixel --test shell_motion -- --ignored --nocapture
//! ```
//!
//! `#[ignore]` because this asserts nothing — motion is judged by looking at
//! it. The scale knob stretches the 200ms slide to two seconds so a burst of
//! frames samples the curve instead of catching its two endpoints, and
//! `LUMA_MOTION=on` keeps the harness from snapping the tweens (see
//! `support::Fixture::open`). `LUMA_SHOTS` names the output directory.
#![cfg(feature = "pixel")]

mod support;

use std::path::PathBuf;
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use support::{Clip, Fixture, NAV, TRACK_NAME, VENUE_NAME};

/// Frames per slide, and the wall-clock gap between them — one full stretched
/// transition, sampled ten times.
const SAMPLES: usize = 10;
const GAP_MS: u32 = 200;

fn shots_dir() -> PathBuf {
    let dir = PathBuf::from(
        std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-shell-motion".into()),
    );
    std::fs::create_dir_all(&dir).expect("could not make the shots directory");
    dir
}

fn run(harness: &mut Harness, code: &str) -> serde_json::Value {
    let result = harness.exec(code, Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

/// Fire `action`, then copy out one frame per sample as the region slides.
fn slide(harness: &mut Harness, name: &str, action: &str) {
    run(harness, &format!("app.action({action:?});"));
    for i in 0..SAMPLES {
        let value = run(
            harness,
            &format!("app.frames(1, {{ waitMs: {GAP_MS} }}); app.screenshot()"),
        );
        let from = PathBuf::from(value["path"].as_str().expect("a screenshot has a path"));
        let to = shots_dir().join(format!("{name}-{i}.png"));
        std::fs::copy(&from, &to).expect("could not copy the shot");
        println!("shot {}", to.display());
    }
}

#[test]
#[ignore = "capture, not a gate"]
fn capture() {
    let mut harness = Fixture::new(
        "shell-motion",
        8,
        vec![Clip::new("pattern-strobe", "Strobe", 1.0, 4.0).lane(0)],
    )
    .window(1280.0, 800.0)
    .open(Mode::Pixel);

    // Both regions open: the sidebar has a venue's tracks, the workspace a tab.
    run(
        &mut harness,
        &format!(
            r#"
            {NAV}
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            app.frames(12, {{ waitMs: 60 }});
        "#
        ),
    );

    slide(&mut harness, "sidebar-close", "luma::ToggleSidebar");
    slide(&mut harness, "sidebar-open", "luma::ToggleSidebar");
    slide(&mut harness, "workspace-close", "luma::ToggleWorkspace");
    slide(&mut harness, "workspace-open", "luma::ToggleWorkspace");
}
