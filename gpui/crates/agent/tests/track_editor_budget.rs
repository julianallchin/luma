//! What one frame of the track editor costs while the eye is moving.
//!
//! Julian reports the timeline lagging at full zoom-out. This is that report
//! turned into a number: a track long enough and a score crowded enough to be
//! representative, zoomed all the way out, then scrolled and scrubbed
//! continuously while every frame in between is timed.
//!
//! # Why pixel mode
//!
//! Headless mode's text system is a stand-in that returns invented metrics, so
//! every string on the canvas — the bar numbers, every clip's label — shapes
//! for free there. Shaping is the single most expensive thing this canvas
//! does per glyph, so a headless number would exclude exactly the cost most
//! likely to be the answer. Pixel mode is the same deterministic platform with
//! the real text system plugged in.
//!
//! What neither mode measures is the GPU: handing the built scene to the
//! renderer has no public entry point at the pinned gpui rev (see
//! `app.timings()`). So these are CPU frame times — `run_until_parked` plus
//! the layout/prepaint/paint walk — and the budget is stated against that.
//! That is the half a timeline regression lands in: the scene this canvas
//! builds is a flat list of quads, and its cost is how many of them there are.
//!
//! # Why it is `#[ignore]`
//!
//! It creates a GPU device and asserts a wall-clock percentile, so it is a
//! measurement, not a gate — a loaded CI box would fail it for reasons that
//! have nothing to do with the code. Run it on demand:
//!
//! ```sh
//! cargo test -p gpui-agent --all-features --test track_editor_budget -- --ignored --nocapture
//! ```

#![cfg(all(feature = "app", feature = "pixel"))]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

/// Five minutes, which is a DJ track.
///
/// The length is not incidental to what is being measured. A waveform is
/// `FULL_WAVEFORM_SIZE` (30 000) buckets *whatever* the track's length, so
/// duration sets the bucket density — 100 buckets a second here — and density
/// against the zoom is how many envelope bars land in one column of pixels.
const TRACK_SECONDS: u32 = 300;

/// Clips per lane, and how many lanes. Ninety clips is a busy score rather
/// than an absurd one; at full zoom-out about a sixth of them are on screen,
/// and all ninety are walked every frame by whatever is not culling.
const LANES: i64 = 6;
const CLIPS_PER_LANE: u32 = 15;

/// 120 Hz. The bar is not "smooth enough", it is the refresh rate of the
/// machine this is developed on.
const BUDGET_MS: f64 = 8.33;

/// The web timeline this one replaces, measured scrubbing:
/// `harness/perf/web-dev-2026-08-20.json`. Kept here because "faster than the
/// thing we are replacing" is the migration's whole thesis, and a budget that
/// passed while sitting just under 27 ms would have met the letter of it.
const WEB_P95_MS: f64 = 27.;

fn harness() -> Harness {
    let clips = (0..LANES)
        .flat_map(|lane| {
            (0..CLIPS_PER_LANE).map(move |index| {
                // Staggered by lane so the lanes do not line up into columns,
                // which would make a cull that is wrong per-lane look right.
                let start = f64::from(index) * 18. + f64::from(lane as u32) * 2.;
                Clip::new(
                    format!("pattern-{lane}-{index}"),
                    format!("Clip {lane}-{index}"),
                    start,
                    start + CLIP_SECONDS,
                )
                .lane(lane)
            })
        })
        .collect();
    Fixture::new("track-editor-budget", TRACK_SECONDS, clips).open(Mode::Pixel)
}

/// Open the editor, zoom all the way out, then scroll and scrub.
///
/// Both legs are one continuous gesture rather than a burst of separate ones:
/// what is being measured is the steady-state cost of a frame while the view
/// is moving, and the first frame after a gesture starts pays for things
/// (a first shaping of every label, a first hitbox) that the next fifty do
/// not.
/// `View::MIN_ZOOM` in `track_editor.rs`, in pixels per second: where the
/// wheel stops. Duplicated rather than exported because the editor's zoom
/// limits are its own business — this test only needs to recognise the state.
const MIN_ZOOM: f32 = 25.;

/// How long each seeded clip is. The script reads the zoom back off a clip's
/// drawn width, which needs this.
const CLIP_SECONDS: f64 = 12.;

fn script() -> String {
    SCRIPT.replace("CLIP_SECONDS", &CLIP_SECONDS.to_string())
}

const SCRIPT: &str = r#"
    function open() {
        app.click(app.snapshot().find({ role: "card", label: "Test Venue" }));
        app.frames(8);
        app.click(app.snapshot().find({ role: "row", label: "Aurora" }));
        // Five minutes of audio to decode and render an envelope for, on a
        // runtime gpui does not own.
        app.frames(60, { waitMs: 50 });
    }

    /** Every frame drawn while `run` ran, as total CPU milliseconds. */
    function measure(run) {
        const from = app.frames(1).frame;
        run();
        return app
            .timings()
            .frames.filter((f) => f.frame > from)
            .map((f) => ({ total: f.parkedMs + f.drawMs, draw: f.drawMs }));
    }

    function waveform() {
        return app.snapshot().find({ role: "card", label: "Waveform" });
    }

    /** Where every clip on the timeline starts, so a leg can prove it moved. */
    function clipStarts() {
        return app
            .snapshot()
            .findAll({ role: "card" })
            .map((c) => c.bounds.x);
    }

    open();

    // All the way out. The zoom is exponential in the wheel distance and
    // clamps at `MIN_ZOOM`, so this overshoots deliberately: the state under
    // test is "as far out as the editor goes", not a particular number.
    app.scroll(waveform(), { dy: -800, steps: 20, modifiers: ["platform"] });

    // A clip is a known number of seconds long, so the widest one on screen
    // reads back the pixels-per-second the view settled at.
    const zoom =
        Math.max(
            ...app
                .snapshot()
                .findAll({ role: "card" })
                .filter((c) => c.label.startsWith("Clip "))
                .map((c) => c.bounds.width),
        ) / CLIP_SECONDS;

    // Scrolling: a bare wheel pans the timeline.
    const beforeScroll = clipStarts();
    const scroll = measure(() =>
        app.scroll(waveform(), { dx: -900, steps: 60 }),
    );
    const scrolled = clipStarts();

    // Scrubbing: press on the waveform and walk the pointer. Every step moves
    // the playhead and repaints.
    const scrub = measure(() =>
        app.drag(waveform(), { dx: 600, dy: 0 }, { steps: 60 }),
    );

    ({
        scroll,
        scrub,
        zoom,
        moved: scrolled.some((x, i) => x !== beforeScroll[i]),
        clips: app.snapshot().findAll({ role: "card" }).length,
        mode: app.timings().mode,
    })
"#;

#[test]
#[ignore = "measures wall-clock frame times on a GPU device; run on demand"]
fn scrolling_and_scrubbing_at_full_zoom_out_stay_inside_the_frame_budget() {
    let mut harness = harness();
    let result = harness.exec(&script(), Duration::from_secs(600));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    assert_eq!(
        out["mode"], "pixel",
        "these numbers are not from pixel mode"
    );
    assert!(
        out["clips"].as_u64().unwrap_or(0) > 1,
        "the editor never drew a timeline: {out:#}"
    );
    assert_eq!(
        out["zoom"].as_f64(),
        Some(f64::from(MIN_ZOOM)),
        "the timeline is not at full zoom-out: {out:#}"
    );
    assert_eq!(
        out["moved"],
        Value::Bool(true),
        "the scroll leg drew sixty frames of a timeline that never moved"
    );

    let scroll = Leg::read(&out["scroll"], "scroll");
    let scrub = Leg::read(&out["scrub"], "scrub");
    println!("\n{scroll}\n{scrub}\n");

    for leg in [&scroll, &scrub] {
        assert!(
            leg.total_p95 <= BUDGET_MS,
            "{} p95 is {:.2} ms, over the {BUDGET_MS} ms budget\n{leg}",
            leg.name,
            leg.total_p95,
        );
        assert!(
            leg.total_p95 < WEB_P95_MS / 2.,
            "{} p95 is {:.2} ms, not decisively under the web's {WEB_P95_MS} ms",
            leg.name,
            leg.total_p95,
        );
    }
}

/// One continuous gesture's frames.
struct Leg {
    name: &'static str,
    /// `parkedMs + drawMs`: everything producing this frame cost the CPU.
    total: Vec<f64>,
    /// The scene build alone, which is the half this canvas controls.
    draw: Vec<f64>,
    total_p95: f64,
}

impl Leg {
    fn read(frames: &Value, name: &'static str) -> Self {
        let frames = frames
            .as_array()
            .unwrap_or_else(|| panic!("{name} produced no frames: {frames:#}"));
        assert!(
            frames.len() >= 30,
            "{name} drew only {} frames, too few to take a percentile of",
            frames.len()
        );
        let field = |key: &str| -> Vec<f64> {
            frames
                .iter()
                .map(|frame| frame[key].as_f64().unwrap_or(f64::NAN))
                .collect()
        };
        let total = field("total");
        Self {
            total_p95: p95(&total),
            total,
            draw: field("draw"),
            name,
        }
    }
}

impl std::fmt::Display for Leg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:>6}  {:>3} frames   p50 {:>6.2}  p95 {:>6.2}  max {:>6.2} ms   (draw p95 {:>6.2} ms)",
            self.name,
            self.total.len(),
            p50(&self.total),
            self.total_p95,
            self.total.iter().copied().fold(0., f64::max),
            p95(&self.draw),
        )
    }
}

/// Nearest-rank percentile: the smallest sample at or above `q` of the way
/// through the sorted run. No interpolation — with fifty-odd frames the
/// interpolated value sits between two real frames and is not one, and the
/// question here is whether a real frame missed the deadline.
fn percentile(samples: &[f64], q: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

fn p50(samples: &[f64]) -> f64 {
    percentile(samples, 0.5)
}

fn p95(samples: &[f64]) -> f64 {
    percentile(samples, 0.95)
}
