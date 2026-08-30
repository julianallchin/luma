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
    const arm = (row) => {
        press("Palette");
        app.click(app.snapshot().find({ role: "row", label: row }));
        app.frames(4);
    };
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
        // Aim *without* committing. A release over the drop surface is a
        // placement, so the drag starts in the page below — where nothing
        // handles a press — and only walks the pointer across the room. The
        // ghost is what the walk solved, and it is still standing there when
        // the button comes up somewhere that does not want it.
        const tray = app.snapshot().find({{ role: "text", label: "TRAY" }});
        app.drag(
            {{ x: tray.bounds.x + 40, y: tray.bounds.y + 20 }},
            {{
                dx: bead.bounds.x + bead.bounds.width / 2 - (tray.bounds.x + 40),
                dy: bead.bounds.y + bead.bounds.height / 2 - (tray.bounds.y + 20),
            }},
            {{ steps: 6 }},
        );
        app.frames(14);
        const ghost = shoot();
        ({{
            before,
            after: camera(),
            empty,
            ghost,
            hand: said().find((l) => l.startsWith("Hand: ")),
            landing: said().find((l) => l.startsWith("Landing: ")),
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
    // Still held: the picture is a ghost, not a placement.
    assert_eq!(out["hand"], "Hand: holding Truss · straight", "{out:#}");
    assert!(
        out["landing"]
            .as_str()
            .is_some_and(|l| l.contains("corner_fl")),
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
            until("the placement to land", (s) =>
                s.findAll({{ role: "text" }}).some((n) => n.label === "Hand: empty"));
            app.frames(10);
        }};
        arm("Truss · straight");
        dropAt(0.34, 0.74);
        arm("Truss · straight");
        dropAt(0.66, 0.74);

        let measured = false;
        for (const bead of sockets().filter((n) => n.label.includes("end_"))) {{
            app.click(bead, {{ restale: "match" }});
            app.frames(6);
            if (said().some((l) => l.startsWith("Gap: "))) {{ measured = true; break; }}
            press("Cancel run");
        }}
        if (!measured) {{ throw new Error("no end measured a gap"); }}
        app.frames(12);
        const accepted = shoot();
        const start = said().filter((l) => l.startsWith("Length ") || l.startsWith("Gap: "));
        for (let i = 0; i < 8; i += 1) {{ press("Longer"); }}
        until("the refusal", (s) =>
            s.findAll({{ role: "text" }}).some((n) => n.label.startsWith("Refused")));
        app.frames(12);
        const refused = shoot();
        ({{
            accepted,
            refused,
            start,
            said: said().filter((l) => l.startsWith("Refused") || l.startsWith("Length ")),
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
    let said = out["said"]
        .as_array()
        .map(|rows| rows.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        said.iter().any(|l| l.starts_with("Refused")),
        "the run was never refused, so the red is not the refusal's\n{out:#}"
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
        const face = app.snapshot().findAll({{ role: "row" }})
            .find((n) => n.label.includes("floor"));
        if (face === undefined) {{ throw new Error("no feature to distribute onto"); }}
        app.click(face, {{ restale: "match" }});
        until("the popup", (s) =>
            s.findAll({{ role: "button" }}).some((n) => n.label === "Distribute"));
        app.frames(10);
        ({{ before, popup: shoot() }})
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
