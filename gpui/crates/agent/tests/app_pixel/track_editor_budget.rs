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
//! CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel \
//!     --test app_pixel track_editor_budget -- --ignored --nocapture
//! ```

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture, TRACK_NAME, VENUE_NAME};

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
        nav.venue("Test Venue");
        app.frames(8);
        nav.track("Aurora");
        // Five minutes of audio to decode and render an envelope for, on a
        // runtime gpui does not own — waited for by its result rather than by
        // a frame count.
        until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
        nav.expand();
        // A test about the editor's own geometry, so give it the whole column:
        // the stage above it would otherwise take 40% of the height.
        nav.stageOff();
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

    /** The 32px ruler strip, which is the only surface that scrubs. */
    function ruler() {
        return app.snapshot().find({ role: "card", label: "Ruler" });
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

    // Scrubbing: press on the *ruler* and walk the pointer. A press on the
    // waveform below it clears the selection instead, which would leave this
    // leg measuring sixty idle frames. Every step moves the playhead and
    // repaints.
    const scrub = measure(() =>
        app.drag(ruler(), { dx: 600, dy: 0 }, { steps: 60 }),
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
    let result = harness.exec(&support::script(&script()), Duration::from_secs(600));
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

/// What a *width tween* costs the timeline, which is a different question from
/// the two legs above.
///
/// Scrolling and scrubbing move the view over a canvas of fixed width. ⌘B
/// moves the canvas's width instead, a pixel at a time for the length of the
/// slide, and the paths keyed on width are the ones that go quadratic there:
/// a preview resample rebuilt per frame (two multi-megabyte buffers and an
/// atlas update per clip on screen), and a deferred round trip through the app
/// per frame to re-ask for a fine waveform already in hand.
///
/// Isolated to the editor's own paint on purpose: the stage is off, so
/// `drawMs` is the timeline plus the shell's chrome and nothing else. The
/// clips are `lit` because a clip with no graph behind it has no heatmap, and
/// the heatmap is half the subject — an unlit fixture would measure a flat
/// fill and report a comfortable number for a canvas that never resampled
/// anything.
///
/// Stated as a ratio against this run's own still stretch for
/// `sidebar_toggle_budget`'s reason: the two stretches are seconds apart on
/// one machine, and only their ratio survives a loaded host.
///
/// ```sh
/// CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel \
///     --test app_pixel track_editor_budget::a_sidebar_slide -- --ignored --nocapture
/// ```
#[test]
#[ignore = "measures wall-clock frame times on a GPU device; run on demand"]
fn a_sidebar_slide_costs_the_timeline_no_cliff_over_holding_still() {
    let clips = (0..TWEEN_LANES)
        .flat_map(|lane| {
            (0..CLIPS_PER_LANE).map(move |index| {
                let start = f64::from(index) * 18. + f64::from(lane as u32) * 2.;
                Clip::new(
                    format!("pattern-{lane}-{index}"),
                    format!("Clip {lane}-{index}"),
                    start,
                    start + CLIP_SECONDS,
                )
                .lane(lane)
                .lit()
            })
        })
        .collect();
    let mut harness = Fixture::new("track-editor-tween", TRACK_SECONDS, clips)
        .with_rig_of(24)
        // The subject is the slide itself, so it has to actually slide: the
        // suite snaps motion by default, which would turn ⌘B into one jump.
        // Stretched, because a frame of this canvas costs about as long as the
        // 270ms sweep gives it — at 1x a sampling walk steps straight over the
        // slide and reads two settled widths. Stretching does not flatter the
        // result: the canvas still gets a width it has not seen on every frame
        // sampled, which is exactly the state under test.
        .with_motion()
        .with_motion_scale(4.)
        .window(2560.0, 1440.0)
        .open(Mode::Pixel);

    let opened = harness.exec(
        &support::script(&format!(
            r#"
            nav.trackEditor({VENUE_NAME:?}, {TRACK_NAME:?});
            until("the timeline", (s) => s.find({{ role: "card", label: "Waveform" }}) !== undefined);
            nav.expand();
            nav.stageOff();
            // The heatmaps are a round trip of their own, and a clip without
            // one paints the flat fill — so the measurement cannot start until
            // at least one has landed.
            until("a decoded heatmap", (s) =>
                s.findAll({{ role: "card" }}).some((n) => n.label.endsWith(" preview")));
            // Zoomed *in*, not out, which is where a resample is expensive:
            // a clip wider than the canvas has its visible slice cut by both
            // canvas edges, so its stretched image is a canvas-width picture
            // — `PREVIEW_TEXELS` of it — and every clip on screen is in that
            // state at once. Zoomed out the same clip resamples a few hundred
            // texels and the whole question is academic.
            const canvas = () =>
                app.snapshot().find({{ role: "card", label: "Waveform" }}).bounds.width;
            const widest = () => Math.max(0, ...app.snapshot().findAll({{ role: "card" }})
                .filter((n) => n.label.startsWith("Clip "))
                .map((n) => n.bounds.width));
            // A clip's node is clipped to the canvas, so "as wide as the
            // canvas" is as much as a walk can read — hence the extra turns
            // past it, which is what puts a clip's *body* across both edges
            // rather than one.
            const zoomIn = () =>
                app.scroll(app.snapshot().find({{ role: "card", label: "Waveform" }}),
                    {{ dy: 120, steps: 5, modifiers: ["platform"] }});
            for (let i = 0; i < 40 && widest() < canvas() * 0.99; i += 1) {{ zoomIn(); }}
            for (let i = 0; i < 8; i += 1) {{ zoomIn(); }}
            app.frames(10, {{ waitMs: 40 }});
            ({{
                previews: app.snapshot().findAll({{ role: "card" }})
                    .filter((n) => n.label.endsWith(" preview")).length,
                widest: Math.round(widest()),
                canvas: Math.round(canvas()),
            }})
        "#
        )),
        Duration::from_secs(600),
    );
    assert_eq!(opened.error, None, "setup failed:\n{}", opened.stdout);
    assert!(
        opened.result["previews"].as_u64().unwrap_or(0) > 0,
        "no clip on screen has a heatmap, so this would measure the flat fill: {:#}",
        opened.result
    );
    assert!(
        opened.result["widest"].as_f64().unwrap_or_default()
            >= opened.result["canvas"].as_f64().unwrap_or(f64::MAX) * 0.99,
        "no clip spans the canvas, so no resample is cut by an edge: {:#}",
        opened.result
    );

    let report = {
        let result = harness.exec(TWEEN, Duration::from_secs(600));
        assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
        result.result
    };
    println!("track editor width tween: {report}");

    let widths = report["widthsSeen"]
        .as_array()
        .expect("widths were sampled");
    assert!(
        widths.len() >= 4,
        "the canvas never slid, so ⌘B was snapped rather than animated: {report}"
    );
    assert!(
        report["slide"]["n"].as_u64().unwrap_or_default() >= 8,
        "too few moving frames to take a median of: {report}"
    );
    let median = Pair::read(&report, "median");
    let p95 = Pair::read(&report, "p95");
    assert!(
        median.still > 0.,
        "the still stretch drew nothing: {report}"
    );

    // Measured here at 2560x1440, three lanes of clips wider than the canvas,
    // before and after `paint_preview`'s chunked resample key and
    // `Editor::fine_need`:
    //
    // |        | median        | p95            |
    // |--------|---------------|----------------|
    // | before | 11.8 (1.41x)  | 66.8 (7.8x)    |
    // | after  |  8.6 (1.03x)  | 30.3 (3.2–3.6x)|
    //
    // Two gates because the two costs show up in different columns. The
    // per-frame resample was a *median* cost — every frame of the slide paid
    // it — while the p95 is the rebuild that a chunk boundary still forces,
    // and it is the one that drops a frame. The tail is not gone: a rebuild
    // is `PREVIEW_TEXELS` of nearest-neighbour per clip on screen, and six of
    // them land in one frame. It is now rare rather than continuous.
    assert!(
        median.ratio() < 1.25,
        "a frame mid-slide cost {:.2}x a still one at the median          ({:.2} ms against {:.2} ms): {report}",
        median.ratio(),
        median.slide,
        median.still,
    );
    assert!(
        p95.ratio() < 5.0,
        "a frame mid-slide cost {:.2}x a still one at p95          ({:.2} ms against {:.2} ms): {report}",
        p95.ratio(),
        p95.slide,
        p95.still,
    );
}

/// One statistic, read off both stretches so they can be compared as a ratio
/// — see the note in `sidebar_toggle_budget` on why the absolute number is
/// not the assertable thing.
struct Pair {
    still: f64,
    slide: f64,
}

impl Pair {
    fn read(report: &Value, statistic: &str) -> Self {
        Self {
            still: report["still"][statistic].as_f64().unwrap_or_default(),
            slide: report["slide"][statistic].as_f64().unwrap_or(f64::MAX),
        }
    }

    fn ratio(&self) -> f64 {
        self.slide / self.still
    }
}

/// Lanes for the tween leg. Fewer than the scrolling legs' six because every
/// clip here carries a graph and a heatmap, and the question is the per-frame
/// cost of the ones on screen rather than how many the walk can cull.
const TWEEN_LANES: i64 = 3;

/// Sample `drawMs` while nothing moves, then across six ⌘B slides, keeping
/// only the frames the canvas actually changed width on.
const TWEEN: &str = r#"
(() => {
    const canvas = () => app.snapshot().find({ role: "card", label: "Waveform" });
    const summarise = (v) => {
        const sorted = [...v].sort((a, b) => a - b);
        const at = (q) => sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))];
        return { n: sorted.length, median: at(0.5), p95: at(0.95), max: sorted[sorted.length - 1] };
    };
    // One sample: the frames a short wait drew, and how wide the canvas ended
    // up. Reading the width *after* the frames is what lets the caller throw
    // away the settled tail of a slide.
    const step = () => {
        const from = app.snapshot().frame;
        // One frame and no wait: a frame here costs about what it is being
        // asked to measure, and the sweep is 270ms — sampling any slower walks
        // straight past the slide and reports two settled widths.
        app.frames(1, { waitMs: 0 });
        return {
            width: Math.round(canvas().bounds.width),
            draw: app.timings().frames.filter((f) => f.frame > from).map((f) => f.drawMs),
        };
    };

    const still = [];
    for (let i = 0; i < 60; i += 1) { still.push(...step().draw); }

    const widths = [];
    const slide = [];
    for (let round = 0; round < 6; round += 1) {
        app.key("cmd-b");
        // `SWEEP` is 270ms and a frame here costs more than the 8ms it asks
        // for, so this covers a whole slide and lands settled; the width is
        // what says where the slide ended, not the count.
        let previous = null;
        for (let i = 0; i < 40; i += 1) {
            const sample = step();
            widths.push(sample.width);
            if (previous !== null && sample.width !== previous) { slide.push(...sample.draw); }
            previous = sample.width;
        }
    }

    return {
        still: summarise(still),
        slide: summarise(slide),
        widthsSeen: [...new Set(widths)].sort((a, b) => a - b),
        mode: app.timings().mode,
    };
})()
"#;
