//! Does a playing stage get slower the longer it plays?
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --release --test visualizer_playback_soak -- --nocapture
//! ```
//!
//! `visualizer_playback_budget.rs` measures what a frame costs. This measures
//! whether that cost is *stable*, which is a different failure: a renderer that
//! is merely slow is slow from the first frame, where one that accumulates —
//! a cache with no eviction, a GPU resource dropped Rust-side but never
//! reclaimed, a queue filling faster than it drains — starts fine and ends
//! frozen. Only the second matches "worse and worse until it stops".
//!
//! Frame time alone cannot tell them apart, so this records resident memory
//! next to it. Growth in both is an accumulation; growth in neither over
//! several thousand frames rules one out for the configuration under test.
#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture, NAV, TRACK_NAME, VENUE_NAME};

const SECONDS: u32 = 240;
const RIG: usize = 120;
/// Frames per measured chunk, and how many chunks. Long enough that a leak of
/// a few megabytes a frame is unmistakable by the end.
const CHUNK: usize = 300;
const CHUNKS: usize = 10;

fn harness() -> Harness {
    Fixture::new(
        "visualizer-playback-soak",
        SECONDS,
        vec![Clip::new("pattern-pulse", "Pulse", 0., f64::from(SECONDS)).lit()],
    )
    .with_rig_of(RIG)
    // A full-screen-sized stage, not the suite's default 1200x800. The
    // presentation path allocates and uploads a viewport-sized texture per
    // presented frame, so pixel count is the axis it scales on and the default
    // window understates it by ~4x.
    .window(2560.0, 1440.0)
    .open(Mode::Pixel)
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(900));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

/// Resident set size of this process, in megabytes.
///
/// The harness runs the app in-process, so our own footprint is the app's.
fn resident_mb() -> f64 {
    let pid = std::process::id();
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .expect("ps failed");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1024.0
}

#[test]
fn playing_does_not_get_slower_the_longer_it_plays() {
    let mut harness = harness();
    run(
        &mut harness,
        &format!(
            r#"
            {NAV}
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            until("the clip", (s) => s.find({{ role: "card", label: "Pulse" }}) !== undefined);
            nav.expand();
            app.frames(10, {{ waitMs: 60 }});
            nav.step("the Play button", "button", "Play");
            app.frames(20, {{ waitMs: 55 }});
        "#
        ),
    );

    let mut medians = Vec::new();
    let mut resident = Vec::new();
    for chunk in 0..CHUNKS {
        let report = run(
            &mut harness,
            &format!(
                // The eval context persists between calls, so each chunk runs in
                // its own scope rather than redeclaring the same names.
                r#"
                (() => {{
                    const from = app.snapshot().frame;
                    app.frames({CHUNK}, {{ waitMs: 4 }});
                    const frames = app.timings().frames.filter((f) => f.frame >= from);
                    const draw = frames.map((f) => f.drawMs).sort((a, b) => a - b);
                    const at = (q) => draw[Math.min(draw.length - 1, Math.floor(draw.length * q))];
                    return {{ frames: draw.length, median: at(0.5), p95: at(0.95), max: draw[draw.length - 1] }};
                }})()
            "#
            ),
        );
        let median = report["median"].as_f64().unwrap_or_default();
        let mb = resident_mb();
        medians.push(median);
        resident.push(mb);
        println!(
            "chunk {chunk:>2}: frames={} median={median:.3}ms p95={:.3}ms max={:.3}ms rss={mb:.1}MB",
            report["frames"], report["p95"].as_f64().unwrap_or_default(),
            report["max"].as_f64().unwrap_or_default(),
        );
    }

    let first = medians[0];
    let last = medians[CHUNKS - 1];
    let grew_mb = resident[CHUNKS - 1] - resident[0];
    println!(
        "soak: median {first:.3}ms -> {last:.3}ms ({:+.0}%), rss {:.1}MB -> {:.1}MB ({grew_mb:+.1}MB)",
        (last - first) / first * 100.0,
        resident[0],
        resident[CHUNKS - 1],
    );

    // Both gates are about the *shape* of the curve, not an absolute budget.
    assert!(
        last < first * 2.0,
        "playback got {:.1}x slower over {} frames: {medians:?}",
        last / first,
        CHUNK * CHUNKS
    );
    assert!(
        grew_mb < 512.0,
        "resident memory grew {grew_mb:.1}MB over {} frames: {resident:?}",
        CHUNK * CHUNKS
    );
}
