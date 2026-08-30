#![cfg(all(feature = "app", feature = "pixel"))]
//! The builder's picture: the half the element tree cannot carry.
//!
//! `tests/headless/venue_builder.rs` proves every transition; these prove the
//! *shape* — that a ghost is drawn, that a refused one is red, that a run being
//! measured has a line across it, and that a socket is beaded where the pointer
//! can reach it. A capture is written for each so a human can look (AF2), and
//! each claim is measured off the pixels rather than asserted from the script.
//!
//! The `CAMERA` readout is read either side of every gesture: **building never
//! moves the eye.** A builder that orbited while placing would make the whole
//! ladder unusable, and the six numbers are the only way to say so.

use gpui_agent::{Harness, Mode, GPU_LIVENESS_TIMEOUT};
use image::RgbaImage;
use serde_json::Value;

use super::support::{self, Clip, Fixture};

fn harness(name: &'static str) -> Harness {
    Fixture::new(
        name,
        20,
        vec![Clip::new("pat-glow", "Glow", 2.0, 5.0).lit()],
    )
    .with_rig()
    .window(1400., 900.)
    .open(Mode::Pixel)
}

fn exec(harness: &mut Harness, script: &str) -> Value {
    let result = harness.exec(&support::script(script), GPU_LIVENESS_TIMEOUT);
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

/// Pixels whose red channel dominates both others by a clear margin — the
/// refusal colour, and nothing else in this scene's palette.
fn red_pixels(image: &RgbaImage) -> u32 {
    image
        .pixels()
        .filter(|p| {
            let [r, g, b, a] = p.0;
            a > 200 && u32::from(r) > u32::from(g) + 60 && u32::from(r) > u32::from(b) + 60
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

const OPEN: &str = r#"
    nav.stage("Test Venue");
    nav.expand();
    app.frames(10);
    const said = () => app.snapshot().findAll({ role: "text" }).map((n) => n.label);
    const camera = () => said().find((l) => l.startsWith("CAMERA "));
    const press = (label) => {
        app.click(app.snapshot().find({ role: "button", label }));
        app.frames(4);
    };
    // The dialog is the only way into place mode now. The query is what brings
    // a row into view: the list is longer than the card, and a click on a
    // clipped row is a refusal rather than a miss.
    const arm = (row) => {
        press("Add element");
        until("the dialog", (s) => s.findAll({ role: "input", label: "Search elements" }).length > 0);
        // The first word: enough to narrow the catalog, and a whole token, so
        // the library's own search matches it too — "Luma " with the space does
        // not.
        app.type(app.snapshot().find({ role: "input", label: "Search elements" }),
            row.split(" ")[0]);
        until("the row", (s) => {
            const n = s.findAll({ role: "row" }).find((n) => n.label === row);
            return n !== undefined && n.bounds.height > 0;
        });
        app.click(app.snapshot().find({ role: "row", label: row }));
        app.frames(6);
    };
    // What the hand is proposing, read off the picture rather than a caption.
    const marks = (prefix) => app.snapshot().findAll({ role: "text" })
        .filter((n) => n.label.startsWith(prefix));
    const sockets = () => app.snapshot().findAll({ role: "button" })
        .filter((n) => n.label.startsWith("Socket "));
    const shoot = () => app.screenshot({});
"#;

/// A ghost is drawn where the piece would go, the beads mark where it could
/// go, and neither moves the camera.
#[test]
#[ignore = "capture: needs a GPU and writes PNGs"]
fn a_ghost_and_its_beads_are_drawn_without_moving_the_eye() {
    let mut harness = harness("venue-builder-ghost");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        const before = camera();
        const empty = shoot();
        arm("Truss · straight");
        const bead = sockets().find((n) => n.label.endsWith("corner_fl"));
        if (bead === undefined) {{
            throw new Error("no corner bead: " + sockets().map((n) => n.label).join(", "));
        }}
        // Aim *without* committing. `scroll` walks the pointer to its target
        // and leaves it there (it has to: gpui routes a wheel by where the
        // pointer last was), which is the one call that moves the cursor with
        // no press — and a press over the room is a placement.
        app.scroll(bead, {{ dy: 0 }});
        app.frames(14);
        const ghost = shoot();
        ({{
            before,
            after: camera(),
            empty,
            ghost,
            ghosts: marks("Ghost ").map((n) => n.label),
        }})
    "#
        ),
    );
    let (empty_path, empty) = support::image::keep_in("venue-builder", &out["empty"], "room-empty");
    let (ghost_path, ghost) = support::image::keep_in("venue-builder", &out["ghost"], "ghost");
    eprintln!(
        "venue builder shots:\n  {}\n  {}",
        empty_path.display(),
        ghost_path.display()
    );
    assert_eq!(
        (empty.width(), empty.height()),
        (ghost.width(), ghost.height()),
        "the two shots cover different windows"
    );
    // The eye never moved: six numbers, compared whole.
    assert_eq!(
        out["before"], out["after"],
        "a build gesture moved the camera\n{out:#}"
    );
    // Still held, and settled where it was walked to — read off the mark the
    // picture draws, which is the same node an eye is looking at.
    assert_eq!(
        out["ghosts"],
        serde_json::json!(["Ghost Truss · straight"]),
        "the ghost never settled on the corner it was walked to\n{out:#}"
    );
    let changed = support::image::differing_fraction(&empty, &ghost, support::image::CHANNEL_NOISE);
    assert!(
        changed > 0.002,
        "only {:.4} of the picture changed when a piece was armed — no ghost and no beads \
         reached the renderer\n  {} vs {}",
        changed,
        empty_path.display(),
        ghost_path.display()
    );
}

/// A run longer than its measured gap draws red, and the same run at the gap
/// does not.
#[test]
#[ignore = "capture: needs a GPU and writes PNGs"]
fn a_refused_run_is_red_and_an_accepted_one_is_not() {
    let mut harness = harness("venue-builder-refused");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        // Two sticks put down along the floor leave a gap between their ends.
        // Only a run with something to reach can be refused, so the refusal
        // needs a room to be refused in.
        const dropAt = (fx, fy) => {{
            const pane = app.snapshot().find({{ role: "card", label: "Stage drop surface" }});
            app.drag(
                {{
                    x: pane.bounds.x + pane.bounds.width * fx,
                    y: pane.bounds.y + pane.bounds.height * fy,
                }},
                {{ dx: 1, dy: 0 }},
                {{ steps: 2 }},
            );
            // Place mode is sticky, so the hand is no evidence a placement
            // landed. The room is: a placed piece brings its own sockets, and
            // a bead is the picture saying the node exists.
            until("the placement to land", (s) =>
                s.findAll({{ role: "button" }})
                    .filter((n) => n.label.startsWith("Socket ")).length > before);
            app.frames(10);
        }};
        arm("Truss · straight");
        let before = sockets().length;
        dropAt(0.34, 0.74);
        before = sockets().length;
        dropAt(0.66, 0.74);
        // Two sticks from one trip to the dialog: place mode persisted.
        app.key("escape");
        app.frames(4);

        let measured = false;
        for (const bead of sockets().filter((n) => n.label.includes("end_"))) {{
            app.click(bead, {{ restale: "match" }});
            app.frames(6);
            // The gap label rides on the measurement line in the picture.
            if (said().some((l) => l.startsWith("Gap: "))) {{ measured = true; break; }}
            app.key("escape");
            app.frames(4);
        }}
        if (!measured) {{ throw new Error("no end measured a gap"); }}
        app.frames(12);
        const accepted = shoot();
        const start = app.snapshot().findAll({{ role: "slider" }})
            .map((n) => n.label).filter((l) => l.startsWith("stage-length = "));
        // Sweep the length box to its far end. A scrub maps the pointer to a
        // position in its own box, so one drag past the right edge asks for the
        // maximum — which is well past any gap this room has.
        const box_ = app.snapshot().findAll({{ role: "slider" }})
            .find((n) => n.label.startsWith("stage-length = "));
        if (box_ === undefined) {{ throw new Error("no length box: " + said().join(", ")); }}
        // Dragged from a *point*, not from the node: the box writes its own
        // value into its label, so a node re-resolved by label mid-drag is a
        // node that no longer exists. The slider suite makes the same note.
        //
        // Centre to right edge — a scrub maps the pointer to a position in its
        // box and clamps, so that is the maximum, and the gesture never leaves
        // the control. The button that commits the run is right beside it.
        app.drag(
            {{
                x: box_.bounds.x + box_.bounds.width / 2,
                y: box_.bounds.y + box_.bounds.height / 2,
            }},
            {{ dx: box_.bounds.width / 2 - 6, dy: 0 }},
            {{ steps: 8 }},
        );
        // Past the gap is the refusal. What it *looks* like is the ghost going
        // red, which the pixels below measure; what the tree can say is the
        // length that provoked it.
        until("the length to run past the gap", (s) =>
            s.findAll({{ role: "slider" }}).some((n) => {{
                const m = /stage-length = ([0-9.]+)/.exec(n.label);
                return m !== null && parseFloat(m[1]) > 2.0;
            }}));
        app.frames(12);
        const refused = shoot();
        ({{
            accepted,
            refused,
            start,
            length: app.snapshot().findAll({{ role: "slider" }})
                .map((n) => n.label).filter((l) => l.startsWith("stage-length = ")),
        }})
    "#
        ),
    );
    let (ok_path, ok) = support::image::keep_in("venue-builder", &out["accepted"], "run-accepted");
    let (bad_path, bad) = support::image::keep_in("venue-builder", &out["refused"], "run-refused");
    eprintln!(
        "run shots:\n  {}\n  {}",
        ok_path.display(),
        bad_path.display()
    );
    // The refusal is not a sentence any more — it is the ghost turning red,
    // which is what the pixels below measure. What the tree still owes is the
    // length that provoked it, so the two shots are known to differ by the
    // gesture and not by a repaint.
    assert_ne!(
        out["start"], out["length"],
        "the length never moved, so the red is not the refusal's\n{out:#}"
    );
    let before = red_pixels(&ok);
    let after = red_pixels(&bad);
    assert!(
        after > before + 200,
        "a refused run drew {after} red pixels against {before} for an accepted one — the \
         refusal never reached the picture\n  {} vs {}",
        ok_path.display(),
        bad_path.display()
    );
}

/// The distribution popup, over the room.
#[test]
#[ignore = "capture: needs a GPU and writes PNGs"]
fn the_distribution_popup_is_drawn_over_the_room() {
    let mut harness = harness("venue-builder-popup");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        const before = shoot();
        // A fixture in hand, then the face it is laid along: the popover is
        // opened by pointing at the room, which is the only way in.
        arm("Luma Mover");
        const face = sockets().find((n) => n.label.endsWith("Deck top"));
        if (face === undefined) {{ throw new Error("no feature to lay a row along"); }}
        app.click(face, {{ restale: "match" }});
        until("the popover", (s) =>
            s.findAll({{ role: "button" }}).some((n) => n.label === "Place"));
        app.frames(10);
        ({{ before, popup: shoot(), stations: marks("Station ").length }})
    "#
        ),
    );
    let (before_path, before) =
        support::image::keep_in("venue-builder", &out["before"], "popup-before");
    let (popup_path, popup) = support::image::keep_in("venue-builder", &out["popup"], "popup");
    eprintln!(
        "popup shots:\n  {}\n  {}",
        before_path.display(),
        popup_path.display()
    );
    let changed =
        support::image::differing_fraction(&before, &popup, support::image::CHANNEL_NOISE);
    assert!(
        changed > 0.01,
        "only {changed:.4} of the picture changed when the popup opened\n  {} vs {}",
        before_path.display(),
        popup_path.display()
    );
}

/// A fixture in the hand draws a *body*, the way a truss does.
///
/// It used to draw nothing at all: the ghost was keyed off a catalog entry and
/// a fixture has none, so the only thing under the cursor was the element
/// layer's mark — a dot, which says something is held and not what. What a
/// ghost is for is the second question.
#[test]
#[ignore = "capture: needs a GPU and writes PNGs"]
fn a_held_fixture_draws_a_body_over_the_face_it_is_aimed_at() {
    let mut harness = harness("venue-builder-fixture-ghost");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        const before = camera();
        const empty = shoot();
        arm("Luma Mover");
        const face = sockets().find((n) => n.label.endsWith("Deck top"));
        if (face === undefined) {{ throw new Error("no face to aim at"); }}
        // Aim without committing — a press over a face opens the row popover,
        // and a card over the room is not the ghost this is about.
        app.scroll(face, {{ dy: 0 }});
        app.frames(14);
        ({{
            before,
            after: camera(),
            empty,
            held: shoot(),
            ghosts: marks("Ghost ").map((n) => n.label),
        }})
    "#
        ),
    );
    let (empty_path, empty) =
        support::image::keep_in("venue-builder", &out["empty"], "fixture-empty");
    let (held_path, held) = support::image::keep_in("venue-builder", &out["held"], "fixture-ghost");
    eprintln!(
        "fixture ghost shots:\n  {}\n  {}",
        empty_path.display(),
        held_path.display()
    );
    assert_eq!(
        out["before"], out["after"],
        "arming a fixture moved the camera\n{out:#}"
    );
    assert_eq!(
        out["ghosts"],
        serde_json::json!(["Ghost Luma Mover"]),
        "a held fixture proposed no placement at all\n{out:#}"
    );
    // A *body*, not the element layer's mark. The bound is what separates the
    // two: a fixture housing at this range covers thousands of pixels, and the
    // mark over it is a five-pixel dot — some three orders of magnitude less
    // of the frame. Anything in between is a hand holding nothing again.
    let changed = support::image::differing_fraction(&empty, &held, support::image::CHANNEL_NOISE);
    assert!(
        changed > 0.0002,
        "only {changed:.4} of the picture changed when a fixture was armed — a dot's worth, so \
         no body reached the renderer\n  {} vs {}",
        empty_path.display(),
        held_path.display()
    );
}
