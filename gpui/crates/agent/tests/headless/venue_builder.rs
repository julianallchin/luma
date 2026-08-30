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
    // What the picture says. The builder publishes no state labels — the spec
    // forbids them — so every claim below is read off the thing that draws it:
    // the ghost, the station marks, the measurement, the beads.
    const said = () => app.snapshot().findAll({ role: "text" }).map((n) => n.label);
    const one = (prefix) => said().find((l) => l.startsWith(prefix));
    const marks = (prefix) => said().filter((l) => l.startsWith(prefix));
    // Press a button, having first walked the pointer onto it.
    //
    // The walk is not decoration. gpui keeps a drag alive until a release
    // lands somewhere the drag did not start, so the first press after a sweep
    // is otherwise spent ending that drag and the button never hears it — and
    // `scroll` with no delta is the only call in the API that moves the
    // pointer without also pressing something.
    const press = (label) => {
        const at = app.snapshot().find({ role: "button", label });
        app.scroll(at, { dy: 0 });
        app.click(app.snapshot().find({ role: "button", label }));
        app.frames(4);
    };
    // A segmented choice is a toggle, not a button — one track, one of N on.
    const pick = (label) => {
        app.click(app.snapshot().find({ role: "toggle", label }));
        app.frames(4);
    };
    // Poll until the app agrees, in blocks of frames rather than one at a
    // time.
    //
    // `until` exists for exactly this and is the right tool for a *load*, but
    // it steps one frame per poll and a builder verb is two round trips deep —
    // the attach, then the far-end check spawned from inside its answer. A
    // one-frame step does not reliably carry the second one back, so the loop
    // that polls fastest is the one that waits longest. Four frames a poll is
    // what makes it deterministic.
    const settle = (what, pred) => {
        for (let i = 0; i < 160; i += 1) {
            if (pred(app.snapshot())) { return; }
            app.frames(4);
        }
        throw new Error("never settled on " + what + ": " + JSON.stringify(
            app.snapshot().nodes.map((n) => n.role + ":" + n.label)));
    };
    // The add-element dialog is the only way into place mode. The query is what
    // brings a row into view: the list is longer than the card, and a click on
    // a clipped row is a refusal rather than a miss.
    const arm = (row) => {
        press("Add element");
        until("the dialog", (s) =>
            s.findAll({ role: "input", label: "Search elements" }).length > 0);
        // The first word: enough to narrow the catalog, and a whole token, so
        // the library's own search matches it too — "Luma " with the space
        // does not.
        app.type(app.snapshot().find({ role: "input", label: "Search elements" }),
            row.split(" ")[0]);
        until("the row", (s) => {
            const n = s.findAll({ role: "row" }).find((n) => n.label === row);
            return n !== undefined && n.bounds.height > 0;
        });
        app.click(app.snapshot().find({ role: "row", label: row }));
        app.frames(6);
    };
    // Drop free on the floor at a point in the viewport, and wait for the round
    // trip. Place mode is sticky, so the hand is no evidence a placement
    // landed — the room is: a placed piece brings its own sockets, and a bead
    // is the picture saying the node exists.
    const dropAt = (fx, fy) => {
        const was = sockets().length;
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
        settle("the placement to land", () => sockets().length > was);
        app.frames(8);
    };
    // Click a node found on an earlier frame. The builder repaints on every
    // readout, so a node caught before a `find` is stale by construction.
    const tap = (node) => {
        app.click(node, { restale: "match" });
        app.frames(4);
    };
    // Sweep a value box to a fraction of its own range. Dragged from a *point*,
    // not from the node: the box writes its value into its label, so a node
    // re-resolved by label mid-drag is one that no longer exists.
    const sweep = (name, fraction) => {
        const box = app.snapshot().findAll({ role: "slider" })
            .find((n) => n.label.startsWith(name + " = "));
        if (box === undefined) { throw new Error("no " + name + ": " + said().join(", ")); }
        const y = box.bounds.y + box.bounds.height / 2;
        app.drag(
            { x: box.bounds.x + 2, y },
            { dx: (box.bounds.width - 4) * fraction, dy: 0 },
            { steps: 8 },
        );
        app.frames(6);
    };
    const sockets = () => app.snapshot().findAll({ role: "button" })
        .filter((n) => n.label.startsWith("Socket "));
    const socket = (suffix) => {
        const found = sockets().find((n) => n.label.endsWith(suffix));
        if (found === undefined) {
            throw new Error("no " + suffix + " bead: " + sockets().map((n) => n.label).join(", "));
        }
        return found;
    };
    // What a value box currently reads, off its own published label.
    const scrub = (name) => {
        const box = app.snapshot().findAll({ role: "slider" })
            .find((n) => n.label.startsWith(name + " = "));
        if (box === undefined) { throw new Error("no " + name + ": " + said().join(", ")); }
        return Number(box.label.slice(name.length + 3));
    };
    // Put a value box on an exact value, without any test knowing the box's
    // range. Two real sweeps calibrate the scale and are *read back*, which is
    // what makes it the control's own claim rather than a constant restated
    // here; the rest is Newton on a straight line, because a scrub lands on its
    // step and one guess can miss by a step.
    //
    // Neither probe is at an end of the travel: a zero-length drag emits no
    // move at all, so `sweep(name, 0)` reads back the value the box already
    // had and calibrates against a number nothing set.
    const setScrub = (name, target) => {
        sweep(name, 0.25);
        const a = settled(name);
        sweep(name, 0.75);
        const b = settled(name);
        if (b === a) { return a; }
        const per = (b - a) / 0.5;
        let f = 0.25 + (target - a) / per;
        for (let i = 0; i < 5; i += 1) {
            sweep(name, Math.min(1, Math.max(0.001, f)));
            const at = settled(name);
            if (at === target) { return at; }
            f += (target - at) / per;
        }
        return settled(name);
    };
    // What a box reads once it has stopped moving. Two facts make this
    // necessary and neither is the drag: some boxes write through a round trip
    // and answer a frame or two later, and gpui holds a drag alive until the
    // release has been drawn — so a press issued straight off a sweep is a
    // press the drag still owns.
    const settled = (name) => {
        let last = scrub(name);
        for (let i = 0; i < 20; i += 1) {
            app.frames(4);
            const now = scrub(name);
            if (now === last) { return now; }
            last = now;
        }
        return last;
    };
    // The context menu on a placed piece, reached the way a person reaches it:
    // a right press on its bead, then the verb.
    const menu = (bead, item) => {
        app.click(bead, { button: "right", restale: "match" });
        until("the menu", (s) =>
            s.findAll({ role: "button" }).some((n) => n.label === item));
        press(item);
    };
"#;

/// The add-element dialog is what arms the hand; nothing else does, and escape
/// is the way back out.
#[test]
fn the_dialog_arms_a_ghost_and_escape_puts_it_down() {
    let mut harness = harness("venue-builder-dialog");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        const before = marks("Ghost ");
        press("Add element");
        until("the dialog", (s) =>
            s.findAll({{ role: "input", label: "Search elements" }}).length > 0);
        const rows = app.snapshot().findAll({{ role: "row" }}).map((n) => n.label);
        app.click(app.snapshot().find({{ role: "row", label: "Truss · straight" }}));
        app.frames(6);
        // A held piece owns the pointer: the surface only exists while it does.
        const surface = app.snapshot().find({{ role: "card", label: "Stage drop surface" }});
        // The ghost is the hand, so it has to be aimed before it can be seen.
        const bead = sockets().find((n) => n.label.endsWith("corner_fl"));
        app.scroll(bead, {{ dy: 0 }});
        app.frames(6);
        const armed = marks("Ghost ");
        app.key("escape");
        app.frames(6);
        ({{
            before,
            rows,
            armed,
            owned: surface !== undefined,
            after: marks("Ghost "),
            released: app.snapshot().find({{ role: "card", label: "Stage drop surface" }}) === undefined,
        }})
    "#
        ),
    );
    assert_eq!(
        out["before"],
        serde_json::json!([]),
        "something was already in the hand\n{out:#}"
    );
    let rows = strings(&out, "rows");
    assert!(
        rows.iter().any(|r| r == "Truss · tower"),
        "the tower row is missing — a stick and a tower are two catalog rows over one \
         generator\n{out:#}"
    );
    assert!(
        rows.iter().any(|r| r == "Luma Mover"),
        "the dialog offers catalog pieces but not fixtures — it is one list or it is the \
         two menus it replaced\n{out:#}"
    );
    assert_eq!(
        out["armed"],
        serde_json::json!(["Ghost Truss · straight"]),
        "the dialog did not arm a ghost\n{out:#}"
    );
    assert_eq!(
        out["owned"], true,
        "an armed hand did not take the pointer\n{out:#}"
    );
    assert_eq!(
        out["after"],
        serde_json::json!([]),
        "escape did not put the hand down\n{out:#}"
    );
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
        settle("the placement to land", (s) =>
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
        app.key("escape");
        app.frames(4);

        // The end that faces the other stick. Both were seated the same way
        // up, so the ray out of one end meets structure and the rest meet
        // nothing — the widest gap is that one, and which end it is is a fact
        // about the room rather than something this script may assume.
        let measured = null;
        for (const bead of sockets().filter((n) => n.label.includes("end_"))) {{
            tap(bead);
            const gap = one("Gap: ");
            if (gap !== undefined) {{
                const metres = Number(gap.slice(5, gap.indexOf(" m")));
                if (measured === null || metres > measured.metres) {{
                    measured = {{ socket: bead.label, gap, metres }};
                }}
            }}
            // Out of the run and back to nothing held, so the next bead is
            // measured from the same state as this one.
            app.key("escape");
            app.frames(4);
        }}
        if (measured === null) {{
            throw new Error("no end measured a gap: " + sockets().map((n) => n.label).join(", "));
        }}
        tap(socket(measured.socket.slice("Socket ".length)));
        const started = one("Gap: ");

        // Past the gap: refused, and the commit is unreachable.
        sweep("stage-length", 1);
        const stretched = scrub("stage-length");
        const blocked = app.snapshot().find({{ role: "button", label: "Place run" }}).enabled;

        // Back to exactly the gap, and place it.
        const restored = setScrub("stage-length", measured.metres);
        const live = app.snapshot().find({{ role: "button", label: "Place run" }}).enabled;
        press("Place run");
        settle("the run to land", (s) =>
            s.findAll({{ role: "text" }}).some((n) => n.label.startsWith("Constraint: ")));
        ({{
            measured,
            started,
            stretched,
            blocked,
            restored,
            live,
            constraint: one("Constraint: "),
            edge: one("Edge: "),
        }})
    "#
        ),
    );
    // Feet are display-only; their presence proves the readout is a
    // measurement rather than a label.
    let started = out["started"].as_str().unwrap_or_default();
    assert!(
        started.starts_with("Gap: ") && started.contains(" ft "),
        "clicking a socket did not start a measured run: {started}\n{out:#}"
    );
    let gap = out["measured"]["metres"].as_f64().unwrap_or_default();
    let stretched = out["stretched"].as_f64().unwrap_or_default();
    assert!(
        stretched > gap,
        "the length box would not go past the {gap} m gap\n{out:#}"
    );
    assert_eq!(
        out["blocked"], false,
        "a run longer than the measured gap could still be committed\n{out:#}"
    );
    // The gap the ray reported and the length that was restored are the same
    // number, read from two different readouts.
    assert_eq!(
        out["restored"].as_f64(),
        Some(gap),
        "the length would not come back to the measured gap\n{out:#}"
    );
    assert_eq!(
        out["live"], true,
        "a run exactly at the gap was still refused\n{out:#}"
    );
    let constraint = out["constraint"].as_str().unwrap_or_default();
    assert!(
        constraint.contains("satisfied"),
        "bridging the gap left the far end unsatisfied: {constraint}\n{out:#}"
    );
    let edge = out["edge"].as_str().unwrap_or_default();
    assert!(
        edge.contains("end_"),
        "the run wrote no edge out of the end it was measured from: {edge}\n{out:#}"
    );
}

/// A free placement flies: trim moves it, and the number the box comes back
/// with is the one the *solve* now holds, not the one the drag asked for.
#[test]
fn trim_lifts_a_free_placement() {
    let mut harness = harness("venue-builder-trim");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        arm("Truss · straight");
        dropAt(0.5, 0.7);
        app.key("escape");
        app.frames(4);
        const gizmo = one("Gizmo: ");
        // Every readout below is re-rendered from the re-solved venue, so the
        // value the box comes back with is the graph's answer and not the
        // drag's request.
        const lifted = setScrub("stage-trim", 1);
        const relation = one("Edge: ");
        ({{ gizmo, lifted, relation }})
    "#
        ),
    );
    // A piece on the venue's own floor is the gizmo's one case.
    assert_eq!(out["gizmo"], "Gizmo: translate", "{out:#}");
    assert_eq!(
        out["lifted"].as_f64(),
        Some(1.0),
        "the trim box would not reach 1.00 m\n{out:#}"
    );
    let relation = out["relation"].as_str().unwrap_or_default();
    assert!(
        relation.contains("venue floor"),
        "a trimmed piece stopped being seated on the floor: {relation}\n{out:#}"
    );
}

/// ⌘D copies the selected subtree onto the cursor, and clicking a socket
/// places it. Flip mirrors the copy, so the flipped wing bolts to the opposite
/// corner and the graph names that corner.
///
/// That the *compatible* sockets grow and change colour is a claim about
/// paint, and is asserted where paint exists — `app_pixel/venue_builder_pixels`.
#[test]
fn duplicate_and_flip_place_a_copy_on_the_opposite_corner() {
    let mut harness = harness("venue-builder-duplicate");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        arm("Truss · straight");
        tap(socket("corner_fl"));
        settle("the first wing", (s) =>
            s.findAll({{ role: "text" }}).some((n) => n.label.startsWith("Edge: ")));
        app.frames(8);
        app.key("escape");
        app.frames(4);
        const first = one("Edge: ");

        app.key("cmd-d");
        app.frames(6);
        // A held piece owns the pointer, which is the picture saying the copy
        // is in the hand — there is no state label to read.
        const held = app.snapshot()
            .find({{ role: "card", label: "Stage drop surface" }}) !== undefined;
        menu(socket("corner_fr"), "Flip");
        const stillHeld = app.snapshot()
            .find({{ role: "card", label: "Stage drop surface" }}) !== undefined;
        tap(socket("corner_fr"));
        settle("the copy to land", (s) =>
            s.findAll({{ role: "text" }}).some((n) =>
                n.label.startsWith("Edge: ") && n.label.includes("corner_fr")));
        app.frames(10);
        ({{ first, held, stillHeld, copy: one("Edge: ") }})
    "#
        ),
    );
    assert_eq!(
        out["held"], true,
        "⌘D put nothing in the hand — nothing claimed the pointer\n{out:#}"
    );
    assert_eq!(
        out["stillHeld"], true,
        "Flip put the copy down instead of turning it over\n{out:#}"
    );
    let first = out["first"].as_str().unwrap_or_default();
    let copy = out["copy"].as_str().unwrap_or_default();
    assert!(
        first.contains("corner_fl") && copy.contains("corner_fr"),
        "the copy did not land on the opposite corner: {first} then {copy}\n{out:#}"
    );
}

/// A fixture carried onto a face is a *row*: the configure popover opens over
/// the point that was clicked, previews one ghost per body, and commits every
/// one of them in a single gesture.
#[test]
fn a_fixture_on_a_face_previews_a_row_and_places_all_of_it() {
    let mut harness = harness("venue-builder-distribute");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        // A stick on the floor, so there is a face to hang a row along.
        arm("Truss · straight");
        dropAt(0.5, 0.72);
        app.key("escape");
        app.frames(4);
        const patched = () => {{
            const line = said().find((l) => l.includes("FIXTURES"));
            return Number(line.slice(0, line.indexOf(" ")));
        }};
        const before = patched();

        arm("Luma Mover");
        tap(socket("face_-y"));
        until("the popover", (s) =>
            s.findAll({{ role: "slider" }}).some((n) => n.label.startsWith("stage-count = ")));
        app.frames(8);
        // The preview is the ghosts, not the number: one station mark per body
        // the row would seat, drawn before anything is committed.
        const wanted = setScrub("stage-count", 4);
        const previewed = marks("Station ").length;
        const fits = one(" will fit");
        press("Place");
        settle("the row to land", () => patched() > before);
        app.frames(10);
        ({{ before, wanted, previewed, fits, after: patched() }})
    "#
        ),
    );
    let wanted = out["wanted"].as_u64().unwrap_or_default();
    assert_eq!(wanted, 4, "the count box would not reach 4\n{out:#}");
    assert_eq!(
        out["previewed"].as_u64(),
        Some(wanted),
        "the popover previewed a different number of bodies than it was set to\n{out:#}"
    );
    let before = out["before"].as_u64().unwrap_or_default();
    assert_eq!(
        out["after"].as_u64(),
        Some(before + wanted),
        "the popover's own count is not what landed in the room\n{out:#}"
    );
}

/// A row that will not fit is refused whole, and the refusal carries the
/// length that would make it fit. Pressing that offer makes the same row fit.
#[test]
fn a_row_that_will_not_fit_is_refused_and_the_offer_makes_it_fit() {
    let mut harness = harness("venue-builder-fit");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        arm("Truss · straight");
        dropAt(0.5, 0.72);
        app.key("escape");
        app.frames(4);
        const patched = () => {{
            const line = said().find((l) => l.includes("FIXTURES"));
            return Number(line.slice(0, line.indexOf(" ")));
        }};
        const before = patched();

        arm("Luma Mover");
        tap(socket("face_-y"));
        until("the popover", (s) =>
            s.findAll({{ role: "slider" }}).some((n) => n.label.startsWith("stage-count = ")));
        app.frames(8);
        // A metre apart, more of them than the stick is long.
        pick("Spacing");
        const wanted = setScrub("stage-count", 24);
        app.frames(6);
        const refusal = said().find((l) => l.includes(" m") && !l.startsWith("Gap: ")
            && !l.includes(" will fit") && l.includes("face"));
        const offer = app.snapshot().findAll({{ role: "button" }})
            .map((n) => n.label).find((l) => l.startsWith("Extend"));
        const blocked = app.snapshot().find({{ role: "button", label: "Place" }}).enabled;
        const previewed = marks("Station ").length;

        press(offer);
        settle("the refit", (s) =>
            s.findAll({{ role: "text" }}).some((n) => n.label.includes(" will fit")));
        app.frames(8);
        const fits = one(String(wanted) + " will fit");
        press("Place");
        settle("the row to land", () => patched() > before);
        app.frames(10);
        ({{ before, wanted, refusal, offer, blocked, previewed, fits, after: patched() }})
    "#
        ),
    );
    let wanted = out["wanted"].as_u64().unwrap_or_default();
    assert!(
        out["refusal"].is_string(),
        "a row too long for its face was not refused in words\n{out:#}"
    );
    let offer = out["offer"].as_str().unwrap_or_default();
    assert!(
        offer.starts_with("Extend") && offer.contains(" m"),
        "the fit failure did not offer the length that would fix it: {offer}\n{out:#}"
    );
    assert_eq!(
        out["blocked"], false,
        "a refused row could still be committed\n{out:#}"
    );
    assert_eq!(
        out["previewed"].as_u64(),
        Some(0),
        "a refused row still previewed bodies\n{out:#}"
    );
    // The extend the refusal offered is what made the same row fit, and the
    // count never changed — measured, not restated.
    assert!(
        out["fits"].is_string(),
        "the extend the refusal offered did not make the row fit\n{out:#}"
    );
    let before = out["before"].as_u64().unwrap_or_default();
    assert_eq!(
        out["after"].as_u64(),
        Some(before + wanted),
        "the row that was quoted as fitting did not all land\n{out:#}"
    );
}

/// Unplaced is the only place a detached piece may live: detaching returns it
/// to the add-element dialog's own section rather than deleting it, and the
/// inspector reports the solve's open ends where the count in the room says
/// there are some.
#[test]
fn detaching_returns_a_piece_to_unplaced_and_the_open_ends_are_reported() {
    let mut harness = harness("venue-builder-unplaced");
    let out = exec(
        &mut harness,
        &format!(
            r#"{OPEN}
        const rows = () => app.snapshot().findAll({{ role: "row" }}).map((n) => n.label);
        // The room before anything is built: the sidebar's own rows, and
        // nothing of the builder's. Everything below is read as what a gesture
        // *added* to this, because `row` is one role over three surfaces.
        const resting = rows();

        arm("Truss · straight");
        dropAt(0.5, 0.7);
        app.key("escape");
        app.frames(4);

        // Selecting the piece opened the sheet, so its unresolved block is up:
        // the sentences the room's own count is a count of.
        const badge = app.snapshot().findAll({{ role: "button" }})
            .map((n) => n.label).find((l) => l.startsWith("Warnings "));
        const unresolved = rows().filter((l) => !resting.includes(l));

        menu(socket("Truss · straight end_a"), "Detach");
        settle("the piece to leave the room", (s) =>
            s.findAll({{ role: "button" }})
                .filter((n) => n.label.startsWith("Socket Truss")).length === 0);

        press("Add element");
        until("the dialog", (s) =>
            s.findAll({{ role: "input", label: "Search elements" }}).length > 0);
        // The section's own name, so the rows under it are the dialog's answer
        // to "what has never been placed" rather than a label match.
        app.type(app.snapshot().find({{ role: "input", label: "Search elements" }}), "Unpl");
        app.frames(8);
        const offered = rows().filter((l) => !resting.includes(l));
        ({{ badge, unresolved, offered }})
    "#
        ),
    );
    let offered = strings(&out, "offered");
    assert!(
        offered.iter().any(|row| row.contains("Truss")),
        "a detached piece did not come back as an unplaced row — it was deleted, not \
         detached\n{out:#}"
    );
    // The count in the room and the sentences in the sheet are the same solve
    // read twice, so they have to agree.
    let badge = out["badge"].as_str().unwrap_or_default();
    let counted: usize = badge
        .trim_start_matches("Warnings ")
        .parse()
        .unwrap_or_else(|_| panic!("no count in {badge:?}: {out:#}"));
    assert_eq!(
        strings(&out, "unresolved").len(),
        counted,
        "the room counted {counted} complaints and the sheet named a different \
         number\n{out:#}"
    );
}
