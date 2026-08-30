//! The patch page, driven through the production app tree.
//!
//! Five properties, and every one of them is about the *backend* being the
//! authority: an address the allocator refuses does not move its row, a
//! collision it computed is what turns cells red, an output binding it stores
//! is what tells universe 17 from universe 1, auto patch reports what it did,
//! and unpatching something standing in the room asks first.
//!
//! # Why this fixture seeds behind the page's back
//!
//! Three of those states are unreachable through the UI by construction —
//! `set_fixture_address` refuses a collision, so no gesture can make one, and a
//! headless host has no Art-Net node to bind. They are seeded straight into the
//! library between [`Fixture::open`] and the script, which is exactly the
//! situation the page has to survive: a database written by a sync pull, a
//! repair, or a second machine.

#![cfg(feature = "app")]

use super::support;

use std::path::Path;
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::Fixture;

const NAME: &str = "venue-patch";

/// The rig, plus what a gesture cannot make: a hand-set collision in universe
/// 1, a fixture out in universe 17, and a binding for each universe.
fn harness(name: &'static str, extras: bool) -> Harness {
    let harness = Fixture::new(name, 20, Vec::new())
        .with_rig()
        .window(1500., 950.)
        .open(Mode::Headless);
    if extras {
        seed_extras(&support::config_dir(name));
    }
    harness
}

fn seed_extras(dir: &Path) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start the seeding runtime")
        .block_on(async {
            let db = luma_lib::database::local::database::init_app_db_at(dir)
                .await
                .expect("failed to reopen the fixture library");
            // `with_rig` patches four movers at 1.1, 1.9, 1.17 and 1.25, eight
            // channels each. This one lands on top of the first two.
            for (id, label, universe, address) in [
                ("fixture-clash", "Clash 1", 1_i64, 5_i64),
                ("fixture-far", "Far 1", 17, 1),
            ] {
                sqlx::query(
                    "INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels,
                                           manufacturer, model, mode_name, fixture_path, label,
                                           pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
                     VALUES (?, NULL, ?, ?, ?, 8, 'Luma', 'Mover', 'Default', ?, ?,
                             0.0, 0.0, 3.0, 0.0, 0.0, 0.0)",
                )
                .bind(id)
                .bind(support::VENUE)
                .bind(universe)
                .bind(address)
                .bind(support::MOVER_PATH)
                .bind(label)
                .execute(&db.0)
                .await
                .expect("failed to seed the extra fixture");
            }
            // Two bindings whose only difference is the node they name. Under
            // the arithmetic this table replaced, `17 & 0xF` and `1 & 0xF` are
            // the same number and these two rows could not exist.
            for (universe, ip, port_address, name) in [
                (1_i64, "10.0.0.5", 0_i64, "Node A"),
                (17, "10.0.0.6", 16, "Node B"),
            ] {
                sqlx::query(
                    "INSERT INTO universe_outputs
                         (universe, node_ip, node_port, port_address, node_name)
                     VALUES (?, ?, 6454, ?, ?)",
                )
                .bind(universe)
                .bind(ip)
                .bind(port_address)
                .bind(name)
                .execute(&db.0)
                .await
                .expect("failed to seed the output binding");
            }
            db.0.close().await;
        });
}

/// Helpers every script here shares: reach the page, and read a row's cells out
/// of the automation tree by the label the row carries.
const HELPERS: &str = r#"
    function texts() {
        return app.snapshot().findAll({ role: "text" }).map((n) => n.label);
    }
    function reading(prefix) {
        const hit = texts().find((l) => l.startsWith(prefix));
        return hit === undefined ? null : hit.slice(prefix.length);
    }
    function rowLabels() {
        return app.snapshot().findAll({ role: "row" }).map((n) => n.label);
    }
    function openPatch() {
        nav.patch("Test Venue");
        // Takeover, and the stage off: the table is nine columns beside a rail,
        // and a cell scrolled out of a shared column is a cell no gesture can
        // reach.
        nav.expand();
        nav.stageOff();
        until("the patch table", (s) => s.find({ role: "row", label: "Mover 0" }) !== undefined);
        app.frames(4);
    }
"#;

fn run(harness: &mut Harness, body: &str) -> Value {
    let script = support::script(&format!("{HELPERS}\n{body}"));
    let result = harness.exec(&script, Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

// ---------------------------------------------------------------------------

/// A typed address the allocator refuses changes nothing and says why.
#[test]
fn a_refused_address_leaves_the_row_where_it_was() {
    let mut harness = harness("venue-patch-refuse", false);
    let out = run(
        &mut harness,
        r#"
        openPatch();
        const before = reading("Mover 1 address = ");

        // Click the address cell, type where Mover 0 already is, commit.
        app.click(app.snapshot().find({ role: "text", label: "Mover 1 address = 9" }));
        until("the address field", (s) =>
            s.findAll({ role: "input" }).some((n) => n.label.startsWith("Mover 1 address = ")));
        app.key("cmd-a 1 enter");
        until("the refusal", (s) =>
            s.findAll({ role: "text" }).some((n) => n.label.indexOf("collides with") >= 0));
        app.frames(4);

        const refusal = texts().find((l) => l.indexOf("collides with") >= 0);
        const after = reading("Mover 1 address = ");
        // No frame between the keystroke and now showed the row anywhere else.
        const everMoved = app.painted().some((s) =>
            s.nodes.some((n) => n.role === "text"
                && n.label.startsWith("Mover 1 address = ")
                && n.label !== "Mover 1 address = 9"));
        ({ before, after, refusal, everMoved })
        "#,
    );
    assert_eq!(out["before"], "9", "{out:#}");
    // The write was refused, so the stored address is the one it always was —
    // read back off the row, not restated from the constant that set it.
    assert_eq!(out["after"], out["before"], "{out:#}");
    assert!(
        out["refusal"]
            .as_str()
            .is_some_and(|line| line.contains("collides with")),
        "no refusal named the conflict: {out:#}"
    );
    assert_eq!(
        out["everMoved"], false,
        "the row moved on some frame before settling back: {out:#}"
    );
}

/// The footprint strip shows the collision the allocator computed, and the
/// universe chips tell 1 from 17.
#[test]
fn the_footprint_reports_a_collision_and_the_universes_it_holds() {
    let mut harness = harness(NAME, true);
    let out = run(
        &mut harness,
        r#"
        openPatch();
        // The strip hangs off the band's chip, one panel at a time.
        app.click(app.snapshot().find({ role: "toggle", label: "Footprint" }));
        until("the footprint", (s) => s.find({ role: "select", label: "Universe 1" }) !== undefined);
        app.frames(4);
        const collisions = texts().filter((l) => l.startsWith("Collision at"));

        app.click(app.snapshot().find({ role: "select", label: "Universe 1" }));
        until("the universe menu", (s) =>
            s.find({ role: "button", label: "Universe 17" }) !== undefined);
        const universes = app.snapshot().findAll({ role: "button" })
            .map((n) => n.label).filter((l) => l.startsWith("Universe "));

        // Universe 17 is a different strip, and it is not colliding.
        app.click(app.snapshot().find({ role: "button", label: "Universe 17" }));
        until("the seventeenth universe", (s) =>
            s.find({ role: "select", label: "Universe 17" }) !== undefined);
        app.frames(4);
        const farCollisions = texts().filter((l) => l.startsWith("Collision at"));
        ({ universes, collisions, farCollisions })
        "#,
    );
    assert_eq!(
        out["universes"],
        serde_json::json!(["Universe 1", "Universe 17"]),
        "{out:#}"
    );
    // `fixture-clash` covers 5..12 across movers at 1..8 and 9..16, so 5..12
    // is claimed twice — reported as the one run it is rather than as eight
    // identical cells.
    assert_eq!(
        out["collisions"],
        serde_json::json!(["Collision at 1.5–12"]),
        "{out:#}"
    );
    assert_eq!(out["farCollisions"], serde_json::json!([]), "{out:#}");
}

/// Universes 1 and 17 resolve to different rows — asserted on what the page
/// resolved, not on the formula that used to alias them.
#[test]
fn universe_one_and_seventeen_are_different_outputs() {
    let mut harness = harness("venue-patch-outputs", true);
    let out = run(
        &mut harness,
        r#"
        openPatch();
        app.click(app.snapshot().find({ role: "toggle", label: "Outputs" }));
        until("the outputs table", (s) =>
            s.findAll({ role: "row" }).some((n) => n.label.startsWith("Universe 1 →")));
        app.frames(4);
        const rows = rowLabels().filter((l) => l.startsWith("Universe "));

        // Unbinding one leaves the other alone, and says what the unbound one
        // now falls back to.
        app.click(app.snapshot().find({ role: "button", label: "Unbind universe 17" }));
        until("the unbound row", (s) =>
            s.findAll({ role: "row" }).some((n) => n.label === "Universe 17 → Not bound"));
        app.frames(4);
        const after = rowLabels().filter((l) => l.startsWith("Universe "));
        const warning = texts().find((l) => l.startsWith("Universe 17 has no node"));
        ({ rows, after, warning })
        "#,
    );
    let rows = out["rows"].as_array().expect("output rows").clone();
    let one = rows
        .iter()
        .find(|row| row.as_str().is_some_and(|l| l.starts_with("Universe 1 →")))
        .expect("universe 1 has a row")
        .as_str()
        .unwrap()
        .to_string();
    let seventeen = rows
        .iter()
        .find(|row| row.as_str().is_some_and(|l| l.starts_with("Universe 17 →")))
        .expect("universe 17 has a row")
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(
        one.trim_start_matches("Universe 1 →"),
        seventeen.trim_start_matches("Universe 17 →"),
        "the two universes resolved to the same output: {out:#}"
    );
    assert_eq!(out["after"].as_array().map(Vec::len), Some(2), "{out:#}");
    // The fallback is named, because a silent one is how the aliasing survived.
    assert!(
        out["warning"]
            .as_str()
            .is_some_and(|line| line.contains("aliases it onto 1")),
        "the unbound universe did not name its fallback: {out:#}"
    );
}

/// Auto Patch re-derives, and says how much it moved.
#[test]
fn auto_patch_moves_and_reports() {
    let mut harness = harness("venue-patch-auto", false);
    let out = run(
        &mut harness,
        r#"
        openPatch();

        // Pin one address by hand so Auto Patch has an override to ask about.
        app.click(app.snapshot().find({ role: "text", label: "Mover 1 address = 9" }));
        until("the address field", (s) =>
            s.findAll({ role: "input" }).some((n) => n.label.startsWith("Mover 1 address = ")));
        app.key("cmd-a 1 0 0 enter");
        until("the moved row", (s) =>
            s.findAll({ role: "text" }).some((n) => n.label === "Mover 1 address = 100"));
        app.frames(4);
        const pinned = reading("Mover 1 address = ");

        app.click(app.snapshot().find({ role: "button", label: "Auto patch" }));
        until("the confirmation", (s) =>
            s.findAll({ role: "text" }).some((n) => n.label.startsWith("Re-derive")));
        const asked = texts().find((l) => l.startsWith("Re-derive"));
        app.click(app.snapshot().findAll({ role: "button", label: "Auto Patch" }).slice(-1)[0]);
        until("the report", (s) =>
            s.findAll({ role: "text" }).some((n) => n.label.startsWith("Auto patch moved")));
        app.frames(6);
        const report = texts().find((l) => l.startsWith("Auto patch moved"));
        const after = reading("Mover 1 address = ");
        ({ pinned, asked, report, after })
        "#,
    );
    assert_eq!(
        out["pinned"], "100",
        "the hand-set address did not land: {out:#}"
    );
    assert!(
        out["asked"].as_str().is_some(),
        "auto patch discarded an override without asking: {out:#}"
    );
    let report = out["report"].as_str().unwrap_or_default().to_string();
    assert!(
        report.contains("discarded 1 overrides"),
        "the report did not count the override it discarded: {out:#}"
    );
    // It moved the pinned row back, which is the whole claim.
    assert_ne!(out["after"], out["pinned"], "{out:#}");
}

/// Unpatching something standing in the room asks before it takes it down.
#[test]
fn unpatching_a_placed_fixture_asks_first() {
    let mut harness = harness("venue-patch-unpatch", false);
    let out = run(
        &mut harness,
        r#"
        openPatch();
        const before = rowLabels().filter((l) => l.startsWith("Mover "));
        const placed = reading("Mover 2 placement = ");

        app.click(app.snapshot().find({ role: "row", label: "Mover 2" }), { button: "right" });
        until("the row menu", (s) =>
            s.find({ role: "button", label: "Unpatch" }) !== undefined);
        app.click(app.snapshot().find({ role: "button", label: "Unpatch" }));
        until("the confirmation", (s) =>
            s.findAll({ role: "text" }).some((n) => n.label.startsWith("Unpatch this fixture")));
        const asked = texts().find((l) => l.startsWith("Unpatch this fixture"));
        // Cancelling is not a quiet yes.
        app.click(app.snapshot().find({ role: "button", label: "Cancel" }));
        app.frames(8);
        const cancelled = rowLabels().filter((l) => l.startsWith("Mover "));

        app.click(app.snapshot().find({ role: "row", label: "Mover 2" }), { button: "right" });
        until("the row menu again", (s) =>
            s.find({ role: "button", label: "Unpatch" }) !== undefined);
        app.click(app.snapshot().find({ role: "button", label: "Unpatch" }));
        until("the confirmation again", (s) =>
            s.findAll({ role: "text" }).some((n) => n.label.startsWith("Unpatch this fixture")));
        app.click(app.snapshot().findAll({ role: "button", label: "Unpatch" }).slice(-1)[0]);
        until("the row to go", (s) =>
            s.find({ role: "row", label: "Mover 2" }) === undefined);
        app.frames(6);
        const after = rowLabels().filter((l) => l.startsWith("Mover "));
        ({ before, placed, asked, cancelled, after })
        "#,
    );
    assert_eq!(out["placed"], "placed", "{out:#}");
    assert!(out["asked"].as_str().is_some(), "{out:#}");
    assert_eq!(
        out["cancelled"], out["before"],
        "cancel unpatched it: {out:#}"
    );
    let before = out["before"].as_array().map(Vec::len).unwrap_or(0);
    let after = out["after"].as_array().map(Vec::len).unwrap_or(0);
    assert_eq!(after + 1, before, "{out:#}");
}

/// The add dialog morphs from the bundle to the count, and what it makes
/// continues the venue's own numbering.
#[test]
fn adding_n_fixtures_morphs_and_continues_the_numbering() {
    let mut harness = harness("venue-patch-add", false);
    let out = run(
        &mut harness,
        r#"
        openPatch();
        const before = rowLabels().filter((l) => l.startsWith("Mover "));

        app.click(app.snapshot().find({ role: "button", label: "Add fixtures" }));
        const bundle = until("the bundle", (s) =>
            s.find({ role: "input", label: "Search fixtures…" }) !== undefined);
        app.type(bundle.find({ role: "input", label: "Search fixtures…" }), "Mover");
        until("the seeded definition", (s) =>
            s.find({ role: "row", label: "Luma Mover" }) !== undefined);
        app.frames(4);
        const library = app.snapshot().findAll({ role: "row" })
            .map((n) => n.label).filter((l) => l.indexOf("Luma") >= 0);

        // The morph: page one leaves, page two arrives, and the table behind
        // the card is never empty on the way.
        app.click(app.snapshot().find({ role: "row", label: "Luma Mover" }));
        // Only the frames that had the page on them: `painted` is a ring of
        // recent frames and reaches back past the tab opening.
        const flight = app.painted()
            .filter((s) => s.find({ role: "card", label: "Add fixtures dialog" }) !== undefined)
            .map((s) => s.nodes.filter((n) =>
                n.role === "row" && n.label.startsWith("Mover ")).length);
        until("the count page", (s) =>
            s.findAll({ role: "input" }).some((n) => n.label.startsWith("count = ")));
        app.frames(6);
        const preview = texts().find((l) => l.startsWith("Lands in"));

        app.click(app.snapshot().find({ role: "input", label: "count = 1" }));
        app.key("cmd-a 3 enter");
        until("the count", (s) => s.find({ role: "input", label: "count = 3" }) !== undefined);
        app.frames(2);
        app.click(app.snapshot().find({ role: "button", label: "Add" }));
        until("three more rows", (s) =>
            s.find({ role: "row", label: "Mover 6" }) !== undefined);
        app.frames(6);
        const after = rowLabels().filter((l) => l.startsWith("Mover "));
        ({ before, library, preview, flight, after })
        "#,
    );
    // The rig ships Mover 0..3, so the mint continues at 4 — the venue's own
    // count, not a fresh one.
    let after: Vec<String> = out["after"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    for expected in ["Mover 4", "Mover 5", "Mover 6"] {
        assert!(
            after.iter().any(|label| label == expected),
            "the batch did not continue the numbering: {out:#}"
        );
    }
    let before = out["before"].as_array().map(Vec::len).unwrap_or(0);
    assert_eq!(after.len(), before + 3, "{out:#}");
    assert!(
        out["preview"]
            .as_str()
            .is_some_and(|line| line.contains("unplaced")),
        "the preview did not say where they land: {out:#}"
    );
    // The card never resizes between routes, so nothing under it re-lays out:
    // the table keeps every row it had for the whole flight.
    let flight = out["flight"].as_array().expect("flight");
    assert!(!flight.is_empty(), "the morph drew no frames: {out:#}");
    assert!(
        flight
            .iter()
            .all(|count| count.as_u64() == Some(before as u64)),
        "the table flashed while the dialog morphed: {out:#}"
    );
}
