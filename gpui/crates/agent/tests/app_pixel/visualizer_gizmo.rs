//! The transform gizmo stands on the thing it transforms, and moves it the way
//! it points.
//!
//! ```sh
//! cargo test -p gpui-agent --features pixel --test app_pixel visualizer_gizmo
//! ```
//!
//! Both halves are about *space*, and both were wrong: `object_pose` handed the
//! editor the stored triple — data space, Z-up, unmirrored — while the camera,
//! `Draw::model` and every overlay it is compared against are the renderer's
//! world, which is `(x, -y, z)` of it (`coords::world_from_data`). A widget
//! drawn at `(x, y, z)` for an object at `(x, -y, z)` looks perfectly plausible
//! on a rig that happens to sit on `y = 0` — which is every fixture in the
//! default test rig, and most of a golden venue. Hence `with_skewed_rig`.
//!
//! Pixel-only, and pixels rather than a readout, for one reason: the failure is
//! a *disagreement between two code paths*, and any number the app publishes
//! would be computed by one of them. What the picture holds is the selection
//! cage, drawn by the golden-pinned `overlay.rs` path at the fixture's own
//! pose, and the gizmo, positioned by the path under test. The camera is read
//! back and the projection done here, so the absolute claim ("the widget is
//! where that fixture *is*") does not lean on the app either.
#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::time::Duration;

use glam::{Vec2, Vec3};
use gpui_agent::{Harness, Mode, GPU_LIVENESS_TIMEOUT};
use luma_scene::Camera;
use serde_json::Value;
use support::{Fixture, SKEW_DEPTH_M};

/// Movers this rig patches. Two, so the pivot is a centroid rather than one
/// object's origin — the arithmetic the app does for a real selection.
const MOVERS: usize = 2;

/// Vertical field of view the visualizer's camera uses (`visualizer::FOV_Y_DEG`).
const FOV_Y_DEG: f32 = 50.0;

/// Where the two movers are, in the renderer's world space.
///
/// `seed_rig` patches them at `x = ±0.6`, `y = SKEW_DEPTH_M`, `z = 3` in *data*
/// space; the mirror takes that to `-y`. Their mean is what the gizmo stands
/// on, and the x's cancel — so the expected pivot is a number this test can
/// state outright rather than derive.
fn expected_pivot() -> Vec3 {
    Vec3::new(0.0, -(SKEW_DEPTH_M as f32), 3.0)
}

fn harness(fixture: &'static str) -> Harness {
    Fixture::new(fixture, 20, Vec::new())
        .with_skewed_rig(MOVERS)
        .open(Mode::Pixel)
}

/// Open the venue, turn the camera off axis, and select the whole rig.
///
/// The opening pose is [`luma_scene::View::Front`], where world Y runs straight
/// into the screen and a mirrored Y is worth a pixel or two of perspective —
/// the orbit is what makes the error a picture rather than a rounding. The
/// selection is a shift-marquee across the viewport because there is nothing
/// else to click: a fixture is found by pointing at where it is drawn, which is
/// the very question under test.
const OPEN: &str = r#"
    nav.universe("Test Venue");
    app.frames(6, { waitMs: 60 });

    function camera() {
        const node = app.snapshot().findAll({ role: "text" })
            .find((n) => n.label.startsWith("CAMERA "));
        return node === undefined ? null : node.label;
    }
    function stage() {
        return app.snapshot().find({ role: "card", label: "Stage" }).bounds;
    }
    function shoot() {
        return app.screenshot({ node: app.snapshot().find({ role: "card", label: "Stage" }) });
    }
    function mode(name) {
        app.click(app.snapshot().find({ role: "button", label: name }), { restale: "match" });
        app.frames(3, { waitMs: 60 });
    }
    // The frame-stats readout paints over the viewport and turns red when the
    // stage drops a frame, which is a saturated hue in a room that has none.
    // It says where it is, so the reading below can leave it out.
    function chrome() {
        return app.snapshot().findAll({ role: "text" })
            .filter((n) => n.label.startsWith("LOW ") || n.label.startsWith("FPS "))
            .map((n) => n.bounds);
    }
    until("the stage's camera", () => camera() !== null);

    const view = stage();
    // A full viewport height of drag is a full turn, so an eighth is 45°.
    app.drag({ x: view.x + view.width / 2, y: view.y + view.height / 2 },
             { dx: Math.round(view.height / 8), dy: 0 },
             { steps: 8, restale: "match" });
    app.frames(4, { waitMs: 60 });

    app.drag({ x: view.x + 6, y: view.y + 6 },
             { dx: view.width - 12, dy: view.height * 0.75 },
             { steps: 8, restale: "match", modifiers: ["shift"] });
    app.frames(4, { waitMs: 60 });
    ({ camera: camera(), stage: stage(), chrome: chrome() })
"#;

/// Where the object is, and where the widget is, in one shot.
///
/// The rotate widget is used for the reading rather than the translate one
/// because its rings are *centred* on the pivot: the centroid of a circle's
/// outline is its centre, so no model of the widget's shape is needed. The
/// translate arms all point away from it.
const SHOOT_RINGS: &str = r#"
    mode("Rotate");
    shoot()
"#;

/// Another shot of the same thing, a few frames later.
///
/// The viewport is asynchronous: a script's frame is always one ahead of the
/// picture (see `visualizer.rs`), and on a loaded machine it can be several. So
/// a reading is taken twice and believed when it stops moving, rather than
/// trusting a fixed number of `frames` calls to have been enough — the failure
/// that costs is a shot of the *previous* widget, which is a plausible-looking
/// number tens of pixels from the right one.
const AGAIN: &str = r#"
    app.frames(4, { waitMs: 60 });
    shoot()
"#;

fn run(harness: &mut Harness, code: &str, budget: Duration) -> Value {
    let result = harness.exec(code, budget);
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

/// Shoot until two consecutive readings of the widget agree.
fn settled(harness: &mut Harness, opened: &Value, first: &str) -> Shot {
    let budget = Duration::from_secs(300);
    let mut shot = Shot::new(&run(harness, first, budget), opened);
    for _ in 0..6 {
        let next = Shot::new(&run(harness, AGAIN, budget), opened);
        if gizmo(&shot).0.distance(gizmo(&next).0) < 2.0 {
            return next;
        }
        shot = next;
    }
    panic!("the stage never settled: the gizmo reading is still moving between shots")
}

fn pixels(shot: &Value) -> image::RgbaImage {
    let path = shot["path"].as_str().expect("a screenshot has a path");
    image::open(path)
        .unwrap_or_else(|error| panic!("could not read {path}: {error}"))
        .to_rgba8()
}

/// The camera the app is holding, from the `CAMERA` readout.
fn camera_of(reading: &Value) -> Camera {
    let label = reading.as_str().expect("a CAMERA reading");
    let n: Vec<f32> = label
        .split_whitespace()
        .skip(1)
        .map(|word| word.parse().expect("a CAMERA field is a number"))
        .collect();
    assert_eq!(n.len(), 6, "CAMERA carries six numbers: {label}");
    Camera {
        target: Vec3::new(n[3], n[4], n[5]),
        radius: n[2],
        azimuth: n[0],
        polar: n[1],
        fov_y_deg: FOV_Y_DEG,
        ..Camera::default()
    }
}

/// A world point, in the screenshot's pixels.
fn project(camera: &Camera, world: Vec3, size: Vec2) -> Vec2 {
    let ndc = camera.project(world, size.x / size.y);
    Vec2::new(
        (ndc.x * 0.5 + 0.5) * size.x,
        (1.0 - (ndc.y * 0.5 + 0.5)) * size.y,
    )
}

/// The screenshot, and what in it is not the rendered stage.
///
/// The stage's own chrome — the frame-stats readout — is painted over the
/// viewport and reports its own bounds, so a reading of the *scene* subtracts
/// them rather than pretending a hue is a hue wherever it lands.
struct Shot {
    image: image::RgbaImage,
    /// In screenshot pixels.
    chrome: Vec<[f32; 4]>,
    /// Screenshot pixels per window point.
    dpr: f32,
    /// The stage card's window origin.
    origin: Vec2,
}

impl Shot {
    fn new(shot: &Value, opened: &Value) -> Self {
        let image = pixels(shot);
        let stage = &opened["stage"];
        let number = |value: &Value| value.as_f64().expect("a bound is a number") as f32;
        let origin = Vec2::new(number(&stage["x"]), number(&stage["y"]));
        let dpr = image.width() as f32 / number(&stage["width"]);
        let chrome = opened["chrome"]
            .as_array()
            .expect("the readout's bounds")
            .iter()
            .map(|b| {
                let (x, y) = (number(&b["x"]) - origin.x, number(&b["y"]) - origin.y);
                [
                    x * dpr - 4.0,
                    y * dpr - 4.0,
                    (x + number(&b["width"])) * dpr + 4.0,
                    (y + number(&b["height"])) * dpr + 4.0,
                ]
            })
            .collect();
        Self {
            image,
            chrome,
            dpr,
            origin,
        }
    }

    fn size(&self) -> Vec2 {
        Vec2::new(self.image.width() as f32, self.image.height() as f32)
    }

    /// A screenshot pixel, as a point in the window.
    fn window(&self, at: Vec2) -> Vec2 {
        self.origin + at / self.dpr
    }

    /// Mean position of every scene pixel a predicate accepts, and how many
    /// there were.
    fn centroid(&self, keep: impl Fn([u8; 3]) -> bool) -> (Vec2, u32) {
        let (sum, count) = self.image.enumerate_pixels().fold(
            (Vec2::ZERO, 0u32),
            |(sum, count), (x, y, pixel)| {
                let (x, y) = (x as f32, y as f32);
                let masked = self
                    .chrome
                    .iter()
                    .any(|r| x >= r[0] && x <= r[2] && y >= r[1] && y <= r[3]);
                if !masked && keep([pixel[0], pixel[1], pixel[2]]) {
                    (sum + Vec2::new(x, y), count + 1)
                } else {
                    (sum, count)
                }
            },
        );
        (sum / count.max(1) as f32, count)
    }
}

/// Floor under "this pixel is a saturated overlay colour and not the room".
/// The rig, the grid and the ground are neutral greys and the fixtures are
/// forced near-black, so nothing in an unlit venue reads as a hue.
const HUE: f32 = 1.3;
const LEVEL: f32 = 40.0;

/// Where the gizmo is: the mean of the rotate rings' red, green and blue.
/// A cage is yellow, which is two channels at once and so fails every test
/// here — the two readings cannot contaminate each other.
fn gizmo(shot: &Shot) -> (Vec2, u32) {
    shot.centroid(|[r, g, b]| {
        let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
        [(r, g, b), (g, r, b), (b, r, g)]
            .iter()
            .any(|&(lead, a, c)| lead > LEVEL && lead > a * HUE && lead > c * HUE)
    })
}

/// Where the objects are: the mean of the selection cages, primary yellow and
/// secondary olive alike. Both are `r ≈ g ≫ b`, which no gizmo handle is.
fn cages(shot: &Shot) -> (Vec2, u32) {
    shot.centroid(|[r, g, b]| {
        let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
        r > LEVEL && g > LEVEL && r > b * HUE && g > b * HUE
    })
}

/// A reading is only worth comparing if the shape it came from is in the frame.
fn assert_visible(what: &str, count: u32, shot: &Shot) {
    assert!(
        count > 40,
        "found {count} {what} pixels in a {:?} shot — the affordance is not in the picture, \
         so nothing below is a measurement",
        shot.size()
    );
}

/// The gate: the widget is drawn on the object, not on its mirror image.
#[test]
fn the_gizmo_stands_where_the_selection_is() {
    let mut harness = harness("visualizer-gizmo-alignment");
    let opened = run(&mut harness, &support::script(OPEN), GPU_LIVENESS_TIMEOUT);
    let camera = camera_of(&opened["camera"]);
    let shot = settled(&mut harness, &opened, SHOOT_RINGS);

    let (widget, ring_pixels) = gizmo(&shot);
    let (objects, cage_pixels) = cages(&shot);
    assert_visible("gizmo", ring_pixels, &shot);
    assert_visible("cage", cage_pixels, &shot);

    // Both against the projection, because the two agreeing on the wrong point
    // is a failure this test must not pass: the cage and the gizmo are drawn by
    // different code, and the camera says where the rig actually is.
    let expected = project(&camera, expected_pivot(), shot.size());
    assert!(
        objects.distance(expected) < 12.0,
        "the selection cages are at {objects:?}, but the fixtures project to {expected:?} \
         through the camera the app reports — the reading itself is wrong"
    );
    assert!(
        widget.distance(expected) < 12.0,
        "the gizmo is at {widget:?} for a selection at {expected:?} — {:.0}px away, \
         which is what drawing it in data space while everything else is in world space \
         looks like",
        widget.distance(expected)
    );
}

/// The other half: the handle moves the object, in the direction it points.
#[test]
fn dragging_the_depth_handle_moves_the_rig_that_way() {
    let mut harness = harness("visualizer-gizmo-drag");
    let opened = run(&mut harness, &support::script(OPEN), GPU_LIVENESS_TIMEOUT);
    let camera = camera_of(&opened["camera"]);
    let before = settled(&mut harness, &opened, SHOOT_RINGS);
    let (was, cage_pixels) = cages(&before);
    assert_visible("cage", cage_pixels, &before);

    // The Y arm, because Y is the axis the mirror flips: an X or Z drag lands
    // correctly even through the bug and would prove nothing. The arm is drawn
    // on whichever side of the pivot faces the camera — that is the drawer's
    // rule and the picker's, so it is this test's too.
    let pivot = expected_pivot();
    let eye = camera.position();
    let towards = (eye - pivot).dot(Vec3::Y).signum();
    let scale = luma_scene::gizmo_scale((eye - pivot).length(), FOV_Y_DEG);
    let grip = project(
        &camera,
        pivot + Vec3::Y * (towards * 0.6 * scale),
        before.size(),
    );
    let along = project(&camera, pivot + Vec3::Y * (towards * scale), before.size()) - grip;

    let from = before.window(grip);
    let drag = along.normalize_or_zero() * 60.0 / before.dpr;
    let after = settled(
        &mut harness,
        &opened,
        &format!(
            r#"
            mode("Translate");
            app.drag({{ x: {:.1}, y: {:.1} }}, {{ dx: {:.1}, dy: {:.1} }},
                     {{ steps: 10, restale: "match" }});
            app.frames(4, {{ waitMs: 60 }});
            {SHOOT_RINGS}
        "#,
            from.x, from.y, drag.x, drag.y
        ),
    );
    let (now, cage_pixels) = cages(&after);
    assert_visible("cage", cage_pixels, &after);

    let moved = now - was;
    let direction = along.normalize_or_zero();
    assert!(
        moved.length() > 6.0,
        "the rig did not move: the drag never took hold of the handle ({was:?} \u{2192} {now:?})"
    );
    assert!(
        moved.dot(direction) > 0.0,
        "dragging the +Y arm moved the rig {moved:?}, against the {direction:?} the arm \
         points — a world delta written into data space comes back mirrored"
    );
}
