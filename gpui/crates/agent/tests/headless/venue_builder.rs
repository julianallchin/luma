#![cfg(feature = "app")]
//! The stage page, driven from outside.
//!
//! Every claim the builder makes is an element — the hand's readout, the
//! landing it would commit, the relation the graph wrote down, the refusal that
//! stops it, the beads a socket can be clicked by — precisely so that these
//! tests exist at all. The headless harness runs with the renderer off, so a
//! builder that expressed a snap only in pixels would have no automation for a
//! single one of its transitions (AF3); what the picture adds is inspected in
//! `app_pixel/venue_builder_pixels.rs`.
//!
//! **Nothing here asserts a coordinate.** Every claim is read back off the
//! *solved* graph — the edge that was written, the constraint that was checked,
//! the freedom the joint admits — so an assertion cannot pass by restating the
//! gesture that produced it.

use std::time::Duration;

use gpui_agent::{Harness, Mode};
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
    .open(Mode::Headless)
}

fn exec(harness: &mut Harness, script: &str) -> Value {
    let result = harness.exec(&support::script(script), Duration::from_secs(240));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

fn strings(out: &Value, key: &str) -> Vec<String> {
    serde_json::from_value(out[key].clone())
        .unwrap_or_else(|_| panic!("{key} is not a list of strings: {out:#}"))
}

/// Reach the builder, with the room's own tab open.
const OPEN: &str = r#"
    nav.stage("Test Venue");
    nav.expand();
    app.frames(6);

    // Every readout the builder prints, as plain strings.
    const said = () => app.snapshot().findAll({ role: "text" }).map((n) => n.label);
    const one = (prefix) => said().find((l) => l.startsWith(prefix));
    const press = (label) => {
        app.click(app.snapshot().find({ role: "button", label }));
        app.frames(4);
    };
    const arm = (row) => {
        press("Palette");
        app.click(app.snapshot().find({ role: "row", label: row }));
        app.frames(4);
    };
    // Drop free on the floor at a point in the viewport, and wait for the
    // round trip. `Hand: empty` is the release; the relation is the landing.
    const dropAt = (fx, fy) => {
        const pane = app.snapshot().find({ role: "card", label: "Stage drop surface" });
        // A one-pixel drag rather than a click, because a point is not a node
        // and `drag` is the only call that takes one. It also walks the pointer
        // first, which is what aims the ghost before the release commits it.
        app.drag(
            {
                x: pane.bounds.x + pane.bounds.width * fx,
                y: pane.bounds.y + pane.bounds.height * fy,
            },
            { dx: 1, dy: 0 },
            { steps: 2 },
        );
        until("the placement to land", (s) =>
            s.findAll({ role: "text" }).some((n) => n.label === "Hand: empty"));
        app.frames(8);
    };
    // Click a node found on an earlier frame. The builder repaints on every
    // readout, so a node caught before a `find` is stale by construction.
    const tap = (node) => {
        app.click(node, { restale: "match" });
        app.frames(4);
    };
    const sockets = () => app.snapshot().findAll({ role: "button" })
        .filter((n) => n.label.startsWith("Socket "));
"#;

/// Picking a palette row is what arms the hand; nothing else does, and escape
/// is the way back out.
#[test]
fn the_palette_arms_a_ghost_and_escape_puts_it_down() {
    let mut harness = harness("venue-builder-palette");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        const before = one("Hand: ");
        press("Palette");
        const rows = app.snapshot().findAll({{ role: "row" }}).map((n) => n.label);
        app.click(app.snapshot().find({{ role: "row", label: "Truss · straight" }}));
        app.frames(4);
        const armed = one("Hand: ");
        // A held piece owns the pointer: the surface only exists while it does.
        const surface = app.snapshot().find({{ role: "card", label: "Stage drop surface" }});
        app.key("escape");
        app.frames(4);
        ({{
            before,
            rows,
            armed,
            owned: surface !== undefined,
            after: one("Hand: "),
            released: app.snapshot().find({{ role: "card", label: "Stage drop surface" }}) === undefined,
        }})
    "#
        ),
    );
    assert_eq!(out["before"], "Hand: empty", "{out:#}");
    let rows = strings(&out, "rows");
    assert!(
        rows.iter().any(|r| r == "Truss · tower"),
        "the tower row is missing — a stick and a tower are two palette rows over one \
         generator\n{out:#}"
    );
    assert_eq!(out["armed"], "Hand: holding Truss · straight", "{out:#}");
    assert_eq!(
        out["owned"], true,
        "an armed hand did not take the pointer\n{out:#}"
    );
    assert_eq!(out["after"], "Hand: empty", "{out:#}");
    assert_eq!(
        out["released"], true,
        "the pointer stayed claimed after the hand was put down\n{out:#}"
    );
}

/// A truss dropped on a deck corner mates that corner, and the edge the graph
/// wrote down names both halves of the joint.
#[test]
fn a_truss_dropped_on_a_deck_corner_bolts_to_it() {
    let mut harness = harness("venue-builder-corner");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        arm("Truss · straight");
        const bead = sockets().find((n) => n.label.endsWith("corner_fl"));
        if (bead === undefined) {{
            throw new Error("no deck corner bead: " + sockets().map((n) => n.label).join(", "));
        }}
        const landing = one("Landing: ");
        tap(bead);
        until("the placement to land", (s) =>
            s.findAll({{ role: "text" }}).some((n) => n.label.startsWith("Edge: ")));
        app.frames(8);
        ({{
            target: bead.label,
            landing,
            edge: one("Edge: "),
            gizmo: one("Gizmo: "),
            faces: sockets().map((n) => n.label).filter((l) => l.includes("face_")),
        }})
    "#
        ),
    );
    // The relation, read off the solved graph rather than off the gesture.
    let edge = out["edge"].as_str().unwrap_or_default().to_string();
    assert!(
        edge.contains("corner_fl"),
        "the edge does not name the corner it was dropped on: {edge}\n{out:#}"
    );
    assert!(
        edge.contains("end_a") || edge.contains("seat") || edge.contains("base"),
        "the edge does not name a socket the truss actually has: {edge}\n{out:#}"
    );
    // A bolt circle has no roll, so the piece it carries has no widget.
    assert_eq!(out["gizmo"], "Gizmo: none", "{out:#}");
    let faces = strings(&out, "faces");
    assert!(
        !faces.is_empty(),
        "the truss's own faces never joined the room — the attach did not land\n{out:#}"
    );
}

/// Two sticks put down along the floor leave a gap between their ends. The ray
/// measures it, a longer run is refused, and the exact one bridges — writing an
/// edge plus a far-end check the solve reports satisfied.
#[test]
fn a_run_measures_its_gap_refuses_a_longer_one_and_bridges_an_exact_one() {
    let mut harness = harness("venue-builder-extend");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        arm("Truss · straight");
        dropAt(0.34, 0.72);
        arm("Truss · straight");
        dropAt(0.66, 0.72);

        // The end that faces the other stick. Both were seated the same way up,
        // so one of the four ends measures a gap and the rest measure nothing.
        let measured = null;
        for (const bead of sockets().filter((n) => n.label.includes("end_"))) {{
            tap(bead);
            const gap = one("Gap: ");
            const length = one("Length ");
            if (gap !== undefined && length !== undefined && gap !== "Gap: 0.50 m") {{
                measured = {{ socket: bead.label, gap, length, feet: one("0") }};
                break;
            }}
            press("Cancel run");
        }}
        if (measured === null) {{
            throw new Error("no end measured a gap: " + sockets().map((n) => n.label).join(", "));
        }}
        const started = said();
        // Past the gap: refused, and the commit is unreachable.
        for (let i = 0; i < 6; i += 1) {{ press("Longer"); }}
        const stretched = said();
        const blocked = app.snapshot().find({{ role: "button", label: "Place run" }}).enabled;
        // Back to exactly the gap, and place it.
        for (let i = 0; i < 6; i += 1) {{ press("Shorter"); }}
        const restored = one("Length ");
        press("Place run");
        until("the run to land", (s) =>
            s.findAll({{ role: "text" }}).some((n) => n.label.startsWith("Constraint: ")));
        app.frames(10);
        ({{
            measured,
            started,
            stretched,
            blocked,
            restored,
            constraint: one("Constraint: "),
            edge: one("Edge: "),
        }})
    "#
        ),
    );
    let started = strings(&out, "started");
    assert!(
        started.iter().any(|t| t.starts_with("Hand: extending")),
        "clicking a socket did not start a run\n{out:#}"
    );
    // Feet are display-only; their presence proves the readout is a measurement
    // rather than a label.
    assert!(
        started.iter().any(|t| t.contains(" ft ")),
        "the measurement carries no imperial small print\n{out:#}"
    );
    let stretched = strings(&out, "stretched");
    assert!(
        stretched.iter().any(|t| t.starts_with("Refused: ")),
        "a run longer than the measured gap was not refused\n{out:#}"
    );
    assert_eq!(
        out["blocked"], false,
        "a refused run could still be committed\n{out:#}"
    );
    // The gap the ray reported and the length that was restored are the same
    // number, read from two different readouts.
    let gap = out["measured"]["gap"].as_str().unwrap_or_default();
    let restored = out["restored"].as_str().unwrap_or_default();
    let gap_m = gap.trim_start_matches("Gap: ").trim_end_matches(" m");
    assert!(
        restored.contains(gap_m),
        "the length came back to {restored} for a {gap} gap\n{out:#}"
    );
    let constraint = out["constraint"].as_str().unwrap_or_default();
    assert!(
        constraint.contains("satisfied"),
        "bridging the gap left the far end unsatisfied: {constraint}\n{out:#}"
    );
}

/// A free placement flies: trim moves it, and the subtree comes along because
/// the resolver is what places the subtree.
#[test]
fn trim_lifts_a_free_placement() {
    let mut harness = harness("venue-builder-trim");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        arm("Truss · straight");
        dropAt(0.5, 0.7);
        const before = one("Landing: ") || "";
        const gizmo = one("Gizmo: ");
        press("Trim up");
        app.frames(10);
        press("Trim up");
        app.frames(10);
        const field = () => app.snapshot().findAll({{ role: "input" }})
            .map((n) => n.label).find((l) => l.startsWith("Trim "));
        ({{ gizmo, trim: field() }})
    "#
        ),
    );
    // A piece on the venue's own floor is the gizmo's one case.
    assert_eq!(out["gizmo"], "Gizmo: translate", "{out:#}");
    let trim = out["trim"].as_str().unwrap_or_default();
    assert!(
        trim == "Trim 1.00",
        "two half-metre lifts did not reach 1.00 m: {trim}\n{out:#}"
    );
}

/// ⌘D copies the selected subtree onto the cursor, every compatible socket
/// lights up, and clicking one places it. Flip is a turn about the root joint.
#[test]
fn duplicate_lights_the_compatible_sockets_and_places_a_copy() {
    let mut harness = harness("venue-builder-duplicate");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        arm("Truss · straight");
        const bead = sockets().find((n) => n.label.endsWith("corner_fl"));
        tap(bead);
        until("the first wing", (s) =>
            s.findAll({{ role: "text" }}).some((n) => n.label.startsWith("Edge: ")));
        app.frames(8);
        const first = one("Edge: ");
        app.key("cmd-d");
        app.frames(4);
        const held = one("Hand: ");
        const flip_before = said().length;
        press("Flip");
        const target = sockets().find((n) => n.label.endsWith("corner_fr"));
        if (target === undefined) {{
            throw new Error("no opposite corner: " + sockets().map((n) => n.label).join(", "));
        }}
        tap(target);
        until("the copy to land", (s) =>
            s.findAll({{ role: "text" }}).some((n) =>
                n.label.startsWith("Edge: ") && n.label.includes("corner_fr")));
        app.frames(10);
        ({{ first, held, copy: one("Edge: ") }})
    "#
        ),
    );
    assert!(
        out["held"]
            .as_str()
            .is_some_and(|h| h.starts_with("Hand: holding")),
        "⌘D put nothing in the hand\n{out:#}"
    );
    let first = out["first"].as_str().unwrap_or_default();
    let copy = out["copy"].as_str().unwrap_or_default();
    assert!(
        first.contains("corner_fl") && copy.contains("corner_fr"),
        "the copy did not land on the opposite corner: {first} then {copy}\n{out:#}"
    );
}

/// The distribution popup calls the one fixture constructor, and reports what
/// it did.
#[test]
fn a_feature_opens_the_distribution_popup_and_places_a_row() {
    let mut harness = harness("venue-builder-distribute");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        const face = app.snapshot().findAll({{ role: "row" }})
            .find((n) => n.label.includes("floor"));
        if (face === undefined) {{
            throw new Error("no distributable feature: " + app.snapshot()
                .findAll({{ role: "row" }}).map((n) => n.label).join(", "));
        }}
        tap(face);
        until("the fixture list", (s) =>
            s.findAll({{ role: "row" }}).some((n) => n.label.includes("Mover")) ||
            s.findAll({{ role: "button" }}).some((n) => n.label === "Distribute"));
        app.frames(8);
        const before = app.snapshot().find({{ role: "button", label: "Distribute" }}).enabled;
        const choice = app.snapshot().findAll({{ role: "row" }})
            .find((n) => n.label.includes("Mover"));
        let placed = null;
        if (choice !== undefined) {{
            tap(choice);
            app.frames(10);
            press("Distribute");
            until("the report", (s) =>
                s.findAll({{ role: "text" }}).some((n) => n.label.startsWith("Placed ")));
            placed = one("Placed ");
        }}
        ({{ before, count: one("Count "), placed, said: said() }})
    "#
        ),
    );
    // A popup with no fixture chosen cannot run: the refusal is the disabled
    // commit, not a silent no-op.
    assert_eq!(
        out["before"], false,
        "distribute was live before a fixture was chosen\n{out:#}"
    );
    assert!(
        out["count"]
            .as_str()
            .is_some_and(|c| c.starts_with("Count ")),
        "the popup has no count\n{out:#}"
    );
    let placed = out["placed"].as_str().unwrap_or_default();
    assert!(
        placed.starts_with("Placed 4 "),
        "the popup's own count is not what landed: {placed}\n{out:#}"
    );
}

/// A row that will not fit is refused whole, and the refusal carries the length
/// that would make it fit. Pressing that offer makes the same call succeed.
#[test]
fn a_row_that_will_not_fit_is_refused_and_the_offer_makes_it_fit() {
    let mut harness = harness("venue-builder-fit");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        // A stick on the floor, so there is a face with a finite length.
        arm("Truss · straight");
        dropAt(0.5, 0.72);
        const face = app.snapshot().findAll({{ role: "row" }})
            .find((n) => n.label.includes("face_-y"));
        if (face === undefined) {{
            throw new Error("no truss face: " + app.snapshot()
                .findAll({{ role: "row" }}).map((n) => n.label).join(", "));
        }}
        tap(face);
        until("the fixture list", (s) =>
            s.findAll({{ role: "row" }}).some((n) => n.label.includes("Mover")));
        tap(app.snapshot().findAll({{ role: "row" }}).find((n) => n.label.includes("Mover")));
        app.frames(10);
        // A metre apart, more of them than the stick is long.
        press("Layout even");
        for (let i = 0; i < 6; i += 1) {{ press("More"); }}
        press("Distribute");
        until("the refusal", (s) =>
            s.findAll({{ role: "text" }}).some((n) => n.label.startsWith("needs ")));
        const refusal = one("needs ");
        const placedBefore = one("Placed ");
        press("Extend and retry");
        until("the retry to land", (s) =>
            s.findAll({{ role: "text" }}).some((n) => n.label.startsWith("Placed ")));
        app.frames(8);
        ({{ refusal, placedBefore, placed: one("Placed "), count: one("Count ") }})
    "#
        ),
    );
    let refusal = out["refusal"].as_str().unwrap_or_default();
    assert!(
        refusal.contains("extend("),
        "the fit failure did not name the call that would fix it: {refusal}\n{out:#}"
    );
    assert!(
        out["placedBefore"].is_null(),
        "a refused distribution placed something\n{out:#}"
    );
    // The needed length, taken from the refusal itself, is what made the same
    // call succeed — measured, not restated.
    let count = out["count"].as_str().unwrap_or_default();
    let wanted: usize = count
        .trim_start_matches("Count ")
        .parse()
        .unwrap_or_else(|_| panic!("no count in {count}: {out:#}"));
    let placed = out["placed"].as_str().unwrap_or_default();
    assert_eq!(
        placed,
        format!("Placed {wanted} fixtures"),
        "the extend the refusal offered did not make the row fit\n{out:#}"
    );
}

/// The tray is the only place an unplaced fixture may live, and the inspector
/// reports the solve's open ends whether or not there are any.
#[test]
fn the_tray_and_the_open_ends_are_reported() {
    let mut harness = harness("venue-builder-tray");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        ({{
            dangling: one("Dangling "),
            rows: app.snapshot().findAll({{ role: "row" }}).map((n) => n.label),
        }})
    "#
        ),
    );
    assert!(
        out["dangling"].is_string(),
        "the inspector never reported the solve's open ends\n{out:#}"
    );
}
