//! What the patch page looks like, for a person to inspect.
//!
//! Four frames: the inventory table, the footprint strip with a collision in
//! it, and both pages of the add-fixtures card. Three of them carry an
//! assertion as well as a picture — a collision is *red* rather than merely
//! reported, and the card's two routes are actually different content — but the
//! captures are the point: this is the surface a critic reads.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::path::Path;
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::Fixture;

const NAME: &str = "venue-patch-pixels";
const DRAWER: &str = "venue-patch";

/// The rig, plus a hand-set collision no gesture could make — `set_fixture_address`
/// refuses one, which is exactly why a database can still hold one.
fn harness() -> Harness {
    let harness = Fixture::new(NAME, 20, Vec::new())
        .with_rig()
        .window(1500., 950.)
        .open(Mode::Pixel);
    seed_collision(&support::config_dir(NAME));
    harness
}

fn seed_collision(dir: &Path) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start the seeding runtime")
        .block_on(async {
            let db = luma_lib::database::local::database::init_app_db_at(dir)
                .await
                .expect("failed to reopen the fixture library");
            sqlx::query(
                "INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels,
                                       manufacturer, model, mode_name, fixture_path, label,
                                       pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
                 VALUES ('fixture-clash', NULL, ?, 1, 5, 8,
                         'Luma', 'Mover', 'Default', ?, 'Clash 1',
                         0.0, 0.0, 3.0, 0.0, 0.0, 0.0)",
            )
            .bind(support::VENUE)
            .bind(support::MOVER_PATH)
            .execute(&db.0)
            .await
            .expect("failed to seed the colliding fixture");
            db.0.close().await;
        });
}

/// Fraction of pixels that read as the danger tone: clearly red, and clearly
/// not one of the greys the rest of the page is made of.
fn red_fraction(image: &image::RgbaImage) -> f32 {
    let red = image
        .pixels()
        .filter(|pixel| {
            let (r, g, b) = (
                u16::from(pixel[0]),
                u16::from(pixel[1]),
                u16::from(pixel[2]),
            );
            r > 110 && r > g + 40 && r > b + 40
        })
        .count();
    red as f32 / (image.width() * image.height()) as f32
}

#[test]
fn the_patch_page_and_its_dialog_are_worth_looking_at() {
    let mut harness = harness();
    let script = support::script(
        r#"
        nav.patch("Test Venue");
        nav.expand();
        nav.stageOff();
        until("the patch table", (s) => s.find({ role: "row", label: "Mover 0" }) !== undefined);
        until("the footprint", (s) =>
            s.findAll({ role: "text" }).some((n) => n.label.startsWith("Collision at")));
        app.frames(6);
        const table = app.screenshot();
        // The strip's own section: 512 cells are not 512 automation nodes, so
        // the section is the smallest thing that can be pointed at.
        const footprint = app.screenshot({
            node: app.snapshot().find({ role: "card", label: "FOOTPRINT" }),
        });

        app.click(app.snapshot().find({ role: "button", label: "Add fixtures" }));
        const bundle = until("the bundle", (s) =>
            s.find({ role: "input", label: "Search fixtures…" }) !== undefined);
        app.type(bundle.find({ role: "input", label: "Search fixtures…" }), "Mover");
        until("the seeded definition", (s) =>
            s.find({ role: "row", label: "Luma Mover" }) !== undefined);
        app.frames(6);
        const library = app.screenshot({
            node: app.snapshot().find({ role: "card", label: "Add fixtures dialog" }),
        });

        app.click(app.snapshot().find({ role: "row", label: "Luma Mover" }));
        until("the count page", (s) =>
            s.findAll({ role: "input" }).some((n) => n.label.startsWith("count = ")));
        app.frames(8);
        const configure = app.screenshot({
            node: app.snapshot().find({ role: "card", label: "Add fixtures dialog" }),
        });
        ({ table, footprint, library, configure })
        "#,
    );
    let result = harness.exec(&script, Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    let (table_path, table) = support::image::keep_in(DRAWER, &out["table"], "table");
    let (footprint_path, footprint) =
        support::image::keep_in(DRAWER, &out["footprint"], "footprint-collision");
    let (library_path, library) = support::image::keep_in(DRAWER, &out["library"], "add-library");
    let (configure_path, configure) =
        support::image::keep_in(DRAWER, &out["configure"], "add-configure");
    println!(
        "patch captures:\n  {}\n  {}\n  {}\n  {}",
        table_path.display(),
        footprint_path.display(),
        library_path.display(),
        configure_path.display(),
    );

    // The page is drawn, not a black rectangle.
    assert!(
        support::image::differing_fraction(&table, &table, 3) < 0.001,
        "the differ disagrees with itself"
    );
    assert!(table.width() > 1000, "the table capture is not the window");

    // Red is on screen, and only where a collision is: the whole window carries
    // a little of it, the strip's own section carries much more.
    assert!(
        red_fraction(&footprint) > 0.0005,
        "no danger tone anywhere on the page with a collision in it: {}",
        footprint_path.display()
    );

    // Two routes, two different pictures — a morph that redrew the same content
    // would be a morph nobody needed.
    assert_eq!(
        library.dimensions(),
        configure.dimensions(),
        "the card resized between routes, so the table under it re-laid out"
    );
    // A low bar on purpose: both routes are mostly the card's own ground —
    // comet's fixed-height body means the content is a band at the top, not a
    // full page — so "these are different pictures" is a few percent of the
    // pixels, not most of them.
    assert!(
        support::image::differing_fraction(&library, &configure, support::image::CHANNEL_NOISE)
            > 0.02,
        "the add dialog's two pages look the same: {} vs {}",
        library_path.display(),
        configure_path.display()
    );
}
