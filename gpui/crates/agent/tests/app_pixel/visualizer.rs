//! The 3D stage view draws, and redraws when the camera moves.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test app_pixel visualizer::
//! ```
//!
//! These are modules of the `app_pixel` binary — see `gpui/BUILD.md` — so the
//! file name is a test-name filter, not a `--test` target.
//!
//! Pixel-only for the same reason `pixel.rs` is — headless mode has no
//! renderer, so `app.screenshot()` throws — and doubly so here: the thing under
//! test *is* a picture. A viewport that painted nothing, or painted the same
//! thing whatever the camera did, would pass every node-tree assertion in the
//! suite and still be entirely broken.
//!
//! One test to a fixture: each carries its own library directory on the pump
//! thread's [`luma_ui::runtime::Runtime`], so several coexist in this binary.
//! What still does not coexist is a process-wide cache in the Luma lib keyed on
//! something two fixtures share — see [`support::Fixture::track_hash`].
#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use gpui_agent::{Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;
use support::Fixture;

/// A venue with a rig in it. The 3D view is opened over the track browser,
/// which is the screen that knows a venue and no score.
fn harness(name: &'static str) -> Harness {
    Fixture::new(name, 20, Vec::new())
        .with_rig()
        .open(Mode::Pixel)
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, GPU_LIVENESS_TIMEOUT);
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

/// Open a venue-naming tab, which is what raises the stage over it, and settle
/// enough frames for the rig load and the first GPU frame to land.
///
/// Every acting call carries `restale: "match"`: the viewport asks for an
/// animation frame at the top of every render, so a frame is *always* one
/// behind by the time a script acts on it. That is the redraw working, not a
/// stale click.
fn open_viewport(harness: &mut Harness) {
    run(
        harness,
        &support::script(
            r#"
            // The stage is a view of the tab below it, so a venue-naming
            // tab is what puts one on screen. The patch names a room and
            // no score, which is the unlit rig this test wants.
            nav.patch("Test Venue");
            app.frames(4, { waitMs: 60 });
            nav.expand();
            app.frames(8, { waitMs: 60 });
        "#,
        ),
    );
}

/// Mean luminance of a PNG, and the fraction of pixels that differ from
/// another shot by more than a threshold. Reading the file rather than trusting
/// the harness is the point: this asserts on what was drawn.
fn pixels(path: &str) -> image::RgbaImage {
    image::open(path)
        .unwrap_or_else(|error| panic!("could not read {path}: {error}"))
        .to_rgba8()
}

fn mean_luma(image: &image::RgbaImage) -> f32 {
    let total: f64 = image
        .pixels()
        .map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]))
        .sum();
    (total / (3.0 * f64::from(image.width() * image.height()))) as f32
}

/// The shared diff at the shared noise floor — see `support::image`.
fn differing_fraction(left: &image::RgbaImage, right: &image::RgbaImage) -> f32 {
    support::image::differing_fraction(left, right, support::image::CHANNEL_NOISE)
}

/// The gate: the viewport draws something, the house-light control changes the
/// lighting, and orbiting changes the camera view.
///
/// All three matter and none implies the others. A viewport wired to a stale
/// image passes "non-black" forever; one that renders a fresh frame with the
/// camera ignored passes "not blank" and fails the orbit.
///
/// The lighting half used to go through the renderer lab's `Sun` checkbox. The
/// lab is gone (`docs/design/venue-workspace.md`), and the control that replaced
/// it is the dock's light slider — `House lights` indoors, `Time of day`
/// outdoors — which asks the venue the same question a diagnostic panel used to
/// ask the renderer. Taking it to zero is this room's "sun off".
///
/// # Why every frame diff here is measured against a churn floor
///
/// This scene never repeats. The haze is marched and accumulated temporally, so
/// two shots of the *same* untouched stage, a second apart, already differ over
/// a fifth of the window at [`support::image::CHANNEL_NOISE`] — measured below
/// as `churn`, not assumed. An assertion that "restoring the lights restores the
/// image" to within half a percent of pixels is therefore not available on this
/// renderer at any threshold — the version of it that went through the lab's
/// `Sun` checkbox had the same floor under it and was never run. What is
/// available, and is what that assertion was defending,
/// is a comparison: the gesture must move the frame further than the scene moves
/// itself, and undoing it must bring the frame back. Mean luminance is the
/// stable half of the pair — the churn wobbles it by a fraction of a unit while
/// the house lights halve it — so the restore is stated there as energy
/// recovered.
#[test]
fn orbiting_changes_what_is_drawn() {
    let mut harness = harness("visualizer");
    open_viewport(&mut harness);

    let shot = |harness: &mut Harness| {
        let value = run(harness, "app.screenshot()");
        pixels(value["path"].as_str().expect("a screenshot has a path"))
    };
    /// One settling interval, used after every gesture below *and* to measure
    /// the churn — so the floor and the readings it judges are the same span.
    const SETTLE: &str = r#"app.frames(24, { waitMs: 30 });"#;

    let before = shot(&mut harness);
    assert!(
        mean_luma(&before) > 1.0,
        "the viewport drew a black frame (mean luma {})",
        mean_luma(&before)
    );

    run(&mut harness, SETTLE);
    let settled = shot(&mut harness);
    let churn = differing_fraction(&before, &settled);
    let luma_churn = (mean_luma(&settled) - mean_luma(&before)).abs();

    // Down to zero, then back to full. The fraction is clamped, so a press four
    // pixels past either end of the slider's 112px column is "all the way"; the
    // restoring drag carries the pointer off the widget as well, because the
    // knob's release-hover tooltip would otherwise be a bubble in the restored
    // frame that was not in `before`.
    run(
        &mut harness,
        r#"
            function slider() {
                const node = app.snapshot().find({ role: "slider", label: "House lights" });
                if (!node) { throw new Error("the stage has no house-light slider"); }
                return node;
            }
            function level() {
                const node = app.snapshot().findAll({ role: "text" })
                    .find((n) => n.label.startsWith("House lights = "));
                return node === undefined ? null : node.label;
            }
            app.drag(slider(), { dx: 0, dy: 54 }, { steps: 12, restale: "match" });
            until("the house lights down", () => level() === "House lights = 0%");
        "#,
    );
    run(&mut harness, SETTLE);
    let dark = shot(&mut harness);
    assert!(
        mean_luma(&dark) < mean_luma(&before),
        "taking the house lights down did not lower mean luminance: {:.2} -> {:.2}",
        mean_luma(&before),
        mean_luma(&dark)
    );
    let darkened = differing_fraction(&before, &dark);
    assert!(
        darkened > 0.005 && darkened > churn,
        "taking the house lights down changed {:.3}% of the frame, against {:.3}% \
         the still scene changes by itself",
        darkened * 100.0,
        churn * 100.0
    );

    run(
        &mut harness,
        r#"
            app.drag(slider(), { dx: 0, dy: -140 }, { steps: 12, restale: "match" });
            until("the house lights back up", () => level() === "House lights = 100%");
        "#,
    );
    run(&mut harness, SETTLE);
    let restored = shot(&mut harness);
    let lost = mean_luma(&before) - mean_luma(&dark);
    let missing = (mean_luma(&restored) - mean_luma(&before)).abs();
    assert!(
        missing < 0.1 * lost,
        "restoring the house lights left {:.2} of the {:.2} luminance it took \
         unaccounted for (churn alone moves it {:.2}): {:.2} -> {:.2} -> {:.2}",
        missing,
        lost,
        luma_churn,
        mean_luma(&before),
        mean_luma(&dark),
        mean_luma(&restored)
    );
    assert!(
        differing_fraction(&before, &restored) < darkened,
        "restoring the house lights left the frame no closer to the original \
         than the dark one was: {:.3}% against {:.3}%",
        differing_fraction(&before, &restored) * 100.0,
        darkened * 100.0
    );

    // Left-drag on the viewport is orbit. The drag starts at the centre of the
    // "Stage" node, which is the viewport's own bounds. The camera's own reading
    // is the direct evidence; the frame diff is what says the picture followed
    // it, and it has to clear the churn floor to mean anything.
    let camera = |harness: &mut Harness| {
        run(
            harness,
            r#"(() => {
                const node = app.snapshot().findAll({ role: "text" })
                    .find((n) => n.label.startsWith("CAMERA "));
                if (!node) { throw new Error("the stage publishes no camera"); }
                return node.label;
            })()"#,
        )
    };
    let aimed = camera(&mut harness);
    run(
        &mut harness,
        r#"
            const stage = app.snapshot().find({ role: "card", label: "Stage" });
            if (!stage) { throw new Error("the viewport is not on screen"); }
            app.drag(stage, { dx: 220, dy: 60 }, { steps: 20, restale: "match" });
            app.frames(4, { waitMs: 16 });
        "#,
    );
    assert_ne!(
        aimed,
        camera(&mut harness),
        "the orbit drag did not turn the camera"
    );

    let after = shot(&mut harness);
    let moved = differing_fraction(&before, &after);
    assert!(
        moved > 0.02 && moved > churn,
        "orbiting changed {:.3}% of the frame, against {:.3}% the still scene \
         changes by itself",
        moved * 100.0,
        churn * 100.0
    );
}

/// The frame-stats panel publishes separate CPU and GPU timing.
///
/// The reading is the label, so this matches its shape rather than a fixed name
/// — a node named "Renderer CPU and GPU timing" would satisfy an assertion like
/// this while publishing no numbers. `^CPU` and not the full split, because
/// "CPU/GPU timing unavailable" is the honest reading on a frame the renderer
/// has not timed yet and is still this node doing its job.
///
/// # Why this presses a corner and then checks what it hit
///
/// The fullscreen button (`top: 16px; right: 16px`, 32px square, `occlude`d) is
/// painted over the middle of the folded stats card (`top: 12px; right: 10px`,
/// 45 x 24.5). A click on the stats node's centre — which is all `app.click`
/// can aim at — therefore toggles fullscreen and never unfolds the panel. The
/// press below aims at the card's top-left corner, which is the strip the button
/// leaves, and under load it still lands on the button often enough that the
/// script has to notice, undo it and try again.
///
/// That overlap is a defect in the corner's layout rather than in this test, and
/// fixing it is outside `tests/`. It is the reason this lives in its own fixture
/// instead of inside [`orbiting_changes_what_is_drawn`]: a stray fullscreen
/// toggle costs nothing here and would invalidate every frame comparison there.
#[test]
fn frame_stats_publish_cpu_and_gpu_timing() {
    let mut harness = harness("visualizer-stats");
    open_viewport(&mut harness);
    run(
        &mut harness,
        r#"
            function stats() {
                const node = app.snapshot().find({ role: "toggle", label: "Frame stats" });
                if (!node) { throw new Error("the stage has no frame-stats panel"); }
                return node;
            }
            function unfolded() {
                return app.snapshot().find((n) => n.role === "text" && /^CPU/.test(n.label))
                    !== undefined;
            }
            function full() {
                return app.snapshot().find({ role: "button", label: "Exit fullscreen" })
                    !== undefined;
            }
            const wasFull = full();
            function press() {
                const bounds = stats().bounds;
                app.drag({ x: bounds.x + 3, y: bounds.y + 3 }, { dx: 0, dy: 0 },
                         { steps: 2, restale: "match" });
                app.frames(4, { waitMs: 30 });
                if (full() === wasFull) { return true; }
                // The press slipped onto the fullscreen button painted over
                // this card. Put the layout back and aim again.
                app.click(app.snapshot().find({
                    role: "button",
                    label: wasFull ? "Fullscreen visualizer" : "Exit fullscreen",
                }), { restale: "match" });
                app.frames(4, { waitMs: 30 });
                return false;
            }
            function fold(want) {
                for (let i = 0; i < 10; i++) {
                    if (press() && unfolded() === want) { return; }
                }
                throw new Error(`the frame-stats card would not ${want ? "unfold" : "fold"}`);
            }
            fold(true);
            fold(false);
        "#,
    );
}

/// The idle gate: a still stage stops submitting frames once the temporal
/// haze has settled — the FPS readout says `IDLE` — and a camera drag wakes
/// it. Without the gate a still stage re-marches the haze at display rate for
/// nobody, which is a spinning fan on any laptop.
#[test]
fn a_still_stage_rests_and_a_drag_wakes_it() {
    let mut harness = harness("visualizer-idle");
    open_viewport(&mut harness);

    let rested = run(
        &mut harness,
        r#"
        (() => {
            const idle = () => app.snapshot().find(
                (n) => n.role === "text" && n.label === "FPS IDLE");
            for (let i = 0; i < 120; i++) {
                if (idle()) return "rested";
                app.frames(1, { waitMs: 16 });
            }
            return "never rested";
        })()
        "#,
    );
    assert_eq!(
        rested.as_str(),
        Some("rested"),
        "a still, paused stage kept rendering"
    );

    let woke = run(
        &mut harness,
        r#"
        (() => {
            const stage = app.snapshot().find({ role: "card", label: "Stage" });
            if (!stage) { throw new Error("the viewport is not on screen"); }
            app.drag(stage, { dx: 80, dy: 0 }, { steps: 5, restale: "match" });
            app.frames(3, { waitMs: 16 });
            const idle = app.snapshot().find(
                (n) => n.role === "text" && n.label === "FPS IDLE");
            return idle ? "still idle" : "woke";
        })()
        "#,
    );
    assert_eq!(
        woke.as_str(),
        Some("woke"),
        "a camera drag did not wake the resting stage"
    );
}
