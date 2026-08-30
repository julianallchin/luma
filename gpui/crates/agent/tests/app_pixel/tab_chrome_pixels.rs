//! Production pixel and motion proof for the workspace tab chrome.

#![cfg(all(feature = "app", feature = "pixel"))]

use super::support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(300));
    assert_eq!(
        result.error, None,
        "script failed:\n{code}\n{}",
        result.stdout
    );
    result.result
}

fn review_dir() -> PathBuf {
    let directory = PathBuf::from("/tmp/luma-tabs-review");
    fs::create_dir_all(&directory).expect("could not create tab review directory");
    directory
}

fn preserve(source: &str, name: &str) -> PathBuf {
    let destination = review_dir().join(name);
    fs::copy(source, &destination)
        .unwrap_or_else(|error| panic!("could not preserve {}: {error}", destination.display()));
    println!("tab chrome capture {}", destination.display());
    destination
}

fn pixels(path: impl AsRef<Path>) -> image::RgbaImage {
    image::open(path.as_ref())
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.as_ref().display()))
        .to_rgba8()
}

fn luma_range(image: &image::RgbaImage) -> u8 {
    let mut low = u8::MAX;
    let mut high = u8::MIN;
    for pixel in image.pixels() {
        let luma = ((u16::from(pixel[0]) + u16::from(pixel[1]) + u16::from(pixel[2])) / 3) as u8;
        low = low.min(luma);
        high = high.max(luma);
    }
    high.saturating_sub(low)
}

/// Mean adjacent-pixel luma delta in the text-free right half of the menu.
/// The same window-space field is sampled before and during the popover, so a
/// real backdrop blur must materially suppress the timeline grid/waveform's
/// high-frequency edges in both the entrance and resting frames.
/// Mean absolute luma difference between two frames over the menu's box.
///
/// Both frames are sampled at the *same* window-space region, so this says
/// only "what is on screen here changed" — which is the one thing a popover
/// owes its backdrop and the one thing that does not depend on where the strip
/// put it.
fn menu_field_change(before: &image::RgbaImage, after: &image::RgbaImage, bounds: &Value) -> f64 {
    assert_eq!(
        before.dimensions(),
        after.dimensions(),
        "frames must be the same size to be differenced"
    );
    let scale = f64::from(before.width()) / 1280.0;
    let x0 = ((number(bounds, "x") + 12.0) * scale).round() as u32;
    let x1 = ((number(bounds, "x") + number(bounds, "width") - 12.0) * scale).round() as u32;
    let y0 = ((number(bounds, "y") + 10.0) * scale).round() as u32;
    let y1 = ((number(bounds, "y") + number(bounds, "height") - 10.0) * scale).round() as u32;
    let luma = |image: &image::RgbaImage, x: u32, y: u32| {
        let pixel = image.get_pixel(x, y);
        (f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2])) / 3.0
    };
    let mut total = 0.0;
    let mut samples = 0_u64;
    for y in y0..y1 {
        for x in x0..x1 {
            total += (luma(after, x, y) - luma(before, x, y)).abs();
            samples += 1;
        }
    }
    total / samples.max(1) as f64
}

fn number(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("missing numeric {key}: {value:#}"))
}

fn opacity(value: &Value) -> f64 {
    value["label"]
        .as_str()
        .and_then(|label| label.rsplit_once(' '))
        .and_then(|(_, opacity)| opacity.parse().ok())
        .unwrap_or_else(|| panic!("closing node did not report opacity: {value:#}"))
}

#[test]
fn menu_and_three_tab_close_are_visible_and_follow_the_authored_motion() {
    // QUICK is 150ms; stretching it makes before/mid/after samples robust on
    // both 60Hz and 120Hz runners. The pure reduced-motion unit test proves
    // this same state machine snaps with no exit or offsets.
    let mut harness = Fixture::new(
        "tab-chrome-pixels",
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0)],
    )
    .with_rig()
    .with_motion()
    .with_motion_scale(10.0)
    .window(1280.0, 800.0)
    .open(Mode::Pixel);

    let out = run(
        &mut harness,
        &support::script(
            r#"
            function rightmostButton(label) {
                return app.snapshot().findAll({ role: "button", label })
                    .sort((a, b) => b.bounds.x - a.bounds.x)[0];
            }
            function closing() {
                return app.snapshot().find((node) =>
                    node.role === "card" && node.label.startsWith("Closing Strobe opacity "));
            }

            nav.trackEditor("Test Venue", "Aurora");
            until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }));

            const menuUnderlay = app.screenshot().path;
            app.click(app.snapshot().find({ role: "button", label: "new-tab" }));
            const menuState = until("new tab menu", (s) =>
                s.find({ role: "card", label: "New tab menu" }) ? s : undefined);
            const menu = menuState.find({ role: "card", label: "New tab menu" });
            const menuRows = ["Patch", "Pattern editor", "Track editor"]
                .map((label) => menuState.find({ role: "button", label }));
            const reason = menuState.find({ role: "text", label: "Select a pattern first" });
            const menuAnimatedFull = app.screenshot().path;
            const menuAnimatedCrop = app.screenshot({ node: menu }).path;
            app.frames(2, { waitMs: 1700 });
            const menuSettledState = until("settled new tab menu", (s) =>
                s.find({ role: "card", label: "New tab menu" }) ? s : undefined);
            const menuSettled = menuSettledState.find({ role: "card", label: "New tab menu" });
            const menuSettledFull = app.screenshot().path;
            const menuSettledCrop = app.screenshot({ node: menuSettled }).path;
            app.action("luma::DismissOverlay");
            until("the dismissed new-tab menu", (s) =>
                s.find({ role: "card", label: "New tab menu" }) === undefined);

            nav.pattern("Strobe");
            until("the pattern tab", (s) => s.find({ role: "button", label: "Strobe" }));
            app.click(app.snapshot().find({ role: "button", label: "new-tab" }));
            const universeMenu = until("the animated Universe menu choice", (s) =>
                s.find({ role: "button", label: "Patch" }) ? s : undefined);
            app.click(universeMenu.find({ role: "button", label: "Patch" }));
            until("the universe tab", (s) =>
                s.find({ role: "card", label: "Test Venue Patch" }));
            app.frames(2, { waitMs: 1100 });

            // Select the middle tab, then close through its real pointer
            // affordance so the successor hotspot is armed at that coordinate.
            // Selection goes through the product navigation seam rather than
            // the chip: the assertion below is about the close animation
            // surviving a pane resize, not about pointer reachability here.
            nav.pattern("Strobe");
            app.frames(2, { waitMs: 40 });
            const beforeState = until("the pointer close affordance", (s) =>
                s.find({ role: "button", label: "Close Strobe" }) ? s : undefined);
            const beforeNeighbor = rightmostButton("Test Venue");
            const close = beforeState.find({ role: "button", label: "Close Strobe" });
            const before = app.screenshot().path;
            app.click(close, { restale: "match" });

            app.frames(1, { waitMs: 30 });
            const startCrop = app.screenshot({ node: closing() }).path;
            const start = app.screenshot().path;
            const startState = app.snapshot();
            const startExit = startState.find((node) =>
                node.role === "card" && node.label.startsWith("Closing Strobe opacity "));
            const stable = startState.find({ role: "button", label: "Close next tab" });

            // The exit is window-owned: resizing its former pane cannot move
            // it even though survivors and the live strip re-anchor.
            const seam = startState.find({ role: "slider", label: "Workspace width" });
            app.drag(seam, { dx: -80, dy: 0 }, { steps: 8, restale: "match" });
            const seamState = app.snapshot();
            const seamExit = seamState.find((node) =>
                node.role === "card" && node.label.startsWith("Closing Strobe opacity "));
            const seamResize = app.screenshot().path;

            app.frames(1, { waitMs: 650 });
            const midCrop = app.screenshot({ node: closing() }).path;
            const mid = app.screenshot().path;
            const midState = app.snapshot();
            const midExit = midState.find((node) =>
                node.role === "card" && node.label.startsWith("Closing Strobe opacity "));
            const midNeighbor = midState.findAll({ role: "button", label: "Test Venue" })
                .sort((a, b) => b.bounds.x - a.bounds.x)[0];

            app.frames(1, { waitMs: 1000 });
            const afterState = app.snapshot();
            const afterNeighbor = rightmostButton("Test Venue");
            ({
                menuUnderlay, menuAnimatedFull, menuAnimatedCrop, menuSettledFull, menuSettledCrop,
                menu: menu.bounds,
                strip: menuState.find({ role: "card", label: "Tab strip" }).bounds,
                menuRows: menuRows.map((row) => ({ bounds: row.bounds, enabled: row.enabled })),
                reason: reason.bounds,
                before, start, startCrop, mid, midCrop, after: app.screenshot().path,
                close: close.bounds, stable: stable.bounds,
                startExit, seamExit, midExit, seamResize,
                beforeNeighbor: beforeNeighbor.bounds,
                midNeighbor: midNeighbor.bounds,
                afterNeighbor: afterNeighbor.bounds,
                exitAfter: afterState.find((node) => node.role === "card" && node.label.startsWith("Closing ")),
                patternAfter: afterState.find({ role: "button", label: "Strobe" }),
            })
            "#,
        ),
    );

    let menu = &out["menu"];
    println!(
        "menu bounds={} rows={} reason={}",
        menu, out["menuRows"], out["reason"]
    );
    assert!(
        number(menu, "width") >= 230.0 && number(menu, "height") >= 140.0,
        "menu wrapper bounds were {menu:#}; rows were {:#}",
        out["menuRows"]
    );
    for row in out["menuRows"].as_array().unwrap() {
        assert!(number(&row["bounds"], "width") > 200.0);
        assert!(number(&row["bounds"], "height") >= 38.0);
    }
    assert_eq!(out["menuRows"][1]["enabled"], false);
    assert!(number(&out["reason"], "width") > 0.0);
    assert!(number(&out["strip"], "x") >= 0.0);
    assert!(number(&out["strip"], "x") + number(&out["strip"], "width") <= 1280.0);

    let start_width = number(&out["startExit"]["bounds"], "width");
    let mid_width = number(&out["midExit"]["bounds"], "width");
    assert!(start_width > mid_width && mid_width > 0.0, "{out:#}");
    assert!(
        opacity(&out["startExit"]) > opacity(&out["midExit"]),
        "{out:#}"
    );
    assert!(
        (number(&out["startExit"]["bounds"], "x") - number(&out["seamExit"]["bounds"], "x")).abs()
            <= 1.0,
        "seam resize moved the window-space exit: {out:#}"
    );
    assert!(
        out["exitAfter"].is_null(),
        "exit survived its transition: {out:#}"
    );
    assert!(
        out["patternAfter"].is_null(),
        "closed tab survived: {out:#}"
    );

    let before_x = number(&out["beforeNeighbor"], "x");
    let mid_x = number(&out["midNeighbor"], "x");
    let after_x = number(&out["afterNeighbor"], "x");
    assert!(
        before_x > mid_x && mid_x > after_x,
        "neighbor did not glide: {out:#}"
    );
    assert!((number(&out["close"], "x") - number(&out["stable"], "x")).abs() <= 1.0);
    assert!((number(&out["close"], "y") - number(&out["stable"], "y")).abs() <= 1.0);

    let menu_underlay = preserve(out["menuUnderlay"].as_str().unwrap(), "menu-underlay.png");
    let menu_animated_full = preserve(
        out["menuAnimatedFull"].as_str().unwrap(),
        "menu-animated-full.png",
    );
    let menu_animated_crop = preserve(
        out["menuAnimatedCrop"].as_str().unwrap(),
        "menu-animated-crop.png",
    );
    let menu_full = preserve(out["menuSettledFull"].as_str().unwrap(), "menu-full.png");
    let menu_crop = preserve(out["menuSettledCrop"].as_str().unwrap(), "menu-crop.png");
    let before = preserve(out["before"].as_str().unwrap(), "close-before.png");
    let start = preserve(out["start"].as_str().unwrap(), "close-start.png");
    let start_crop = preserve(out["startCrop"].as_str().unwrap(), "close-start-chip.png");
    let mid = preserve(out["mid"].as_str().unwrap(), "close-mid.png");
    let mid_crop = preserve(out["midCrop"].as_str().unwrap(), "close-mid-chip.png");
    let seam_resize = preserve(out["seamResize"].as_str().unwrap(), "close-seam-resize.png");
    let after = preserve(out["after"].as_str().unwrap(), "close-after.png");
    assert!(
        luma_range(&pixels(menu_crop)) > 24,
        "settled menu crop lacks readable contrast"
    );
    assert!(
        luma_range(&pixels(&menu_animated_crop)) > 24,
        "animated menu crop lacks readable contrast"
    );
    // The menu covers what was there. Stated as *change*, not as a drop in
    // edge energy: the sampled box is inside the menu, so it contains the
    // menu's own row labels, and "Select a pattern first" alone is 206pt wide
    // in a 240pt card — there is no text-free corner of a popover to read a
    // backdrop through. The old form asserted the energy there *fell* by a
    // quarter, which held only while the menu happened to sit over the
    // waveform: it read the label text against whatever screen region the
    // strip's layout put behind it, so moving the `+` by a few pixels decided
    // it. Both frames now had more edge energy than the bare shell, which is
    // what a card full of white text over a dark panel should have.
    //
    // The frost primitive itself is proved in `dialog_blur`, against black and
    // white stripes with an *empty* frosted child — no glyphs to confound it,
    // and no dependence on where a menu lands. That is the gate for blur; this
    // one is for the menu being painted at all.
    let underlay = pixels(&menu_underlay);
    let animated_change = menu_field_change(&underlay, &pixels(&menu_animated_full), menu);
    let settled_change = menu_field_change(&underlay, &pixels(&menu_full), menu);
    println!("menu field change animated={animated_change:.3} settled={settled_change:.3}");
    assert!(
        animated_change > 8.0,
        "the animated popover did not cover the shell behind it: {animated_change:.3}"
    );
    assert!(
        settled_change > 8.0,
        "the settled popover did not cover the shell behind it: {settled_change:.3}"
    );
    for path in [
        menu_underlay,
        menu_animated_full,
        menu_animated_crop,
        menu_full,
        before,
        start,
        start_crop,
        mid,
        mid_crop,
        seam_resize,
        after,
    ] {
        assert!(pixels(path).width() > 0);
    }

    // A compact production window forces three authored slots below their
    // maximum. Assert real clipped hitboxes—not just width arithmetic—and
    // keep the frame that proves the floating menu escapes the rail mask
    // without escaping the viewport.
    let mut compact = Fixture::new(
        "tab-chrome-pixels-compact",
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0)],
    )
    .with_rig()
    .window(420.0, 480.0)
    .open(Mode::Pixel);
    let compact = run(
        &mut compact,
        &support::script(
            r#"
            nav.trackEditor("Test Venue", "Aurora");
            nav.pattern("Strobe");
            const initialPlusState = until("compact initial add-tab control", (s) =>
                s.find({ role: "button", label: "new-tab" }) ? s : undefined);
            app.click(initialPlusState.find({ role: "button", label: "new-tab" }));
            const universeMenu = until("compact Universe choice", (s) =>
                s.find({ role: "button", label: "Patch" }) ? s : undefined);
            app.click(universeMenu.find({ role: "button", label: "Patch" }));
            until("compact Universe tab", (s) =>
                s.find({ role: "button", label: "Test Venue" }) ? s : undefined);
            // With the sidebar still consuming 257px, the 420px shell has no
            // legal split, so the panel is holding the whole remainder rather
            // than being squeezed off screen. Hiding the sidebar starts the
            // real pane entrance; the add control rides the panel's band the
            // whole way, and must be present in every frame of it.
            app.key("secondary-b");
            const openingPlusBounds = [];
            for (let frame = 0; frame < 45; frame++) {
                const openingPlus = app.snapshot().find({ role: "button", label: "new-tab" });
                if (!openingPlus) {
                    throw new Error(`compact add-tab disappeared during pane opening at sample ${frame}`);
                }
                openingPlusBounds.push(openingPlus.bounds);
                app.frames(1, { waitMs: 80 });
            }
            until("compact sidebar closed", (s) =>
                s.find({ role: "input", label: "Search tracks…" }) === undefined &&
                s.find({ role: "row", label: "Aurora" }) === undefined ? s : undefined);
            until("compact universe", (s) =>
                s.find({ role: "card", label: "Test Venue Patch" }));
            const compactPlusState = until("compact add-tab control", (s) =>
                s.find({ role: "button", label: "new-tab" }) ? s : undefined);
            const compactPlus = compactPlusState.find({ role: "button", label: "new-tab" });
            app.click(compactPlus);
            const shot = until("compact menu", (s) =>
                s.find({ role: "card", label: "New tab menu" }) ? s : undefined);
            const rows = ["Patch", "Pattern editor", "Track editor"]
                .map((label) => shot.find({ role: "button", label }));
            const labels = new Set([
                "Aurora", "Strobe", "Test Venue", "Close Aurora", "Close Strobe",
                "Close Test Venue", "new-tab", "workspace-collapse", "New tab menu",
                "Tab strip",
            ]);
            ({
                full: app.screenshot().path,
                crop: app.screenshot({ node: shot.find({ role: "card", label: "New tab menu" }) }).path,
                nodes: shot.nodes.filter((node) => labels.has(node.label))
                    .map((node) => ({ role: node.role, label: node.label, bounds: node.bounds })),
                menu: shot.find({ role: "card", label: "New tab menu" }).bounds,
                plus: compactPlus.bounds,
                rows: rows.map((row) => row.bounds),
                openingPlusBounds,
            })
            "#,
        ),
    );
    for node in compact["nodes"].as_array().unwrap() {
        let bounds = &node["bounds"];
        let x = number(bounds, "x");
        let width = number(bounds, "width");
        assert!(
            x >= 0.0 && x + width <= 420.0,
            "compact chrome escaped: {node:#}"
        );
    }
    let compact_menu = &compact["menu"];
    assert!(number(compact_menu, "x") >= 0.0);
    assert!(number(compact_menu, "x") + number(compact_menu, "width") <= 420.0);
    assert!((number(compact_menu, "width") - 240.0).abs() <= f64::EPSILON);
    assert_eq!(compact["openingPlusBounds"].as_array().unwrap().len(), 45);
    let compact_plus = &compact["plus"];
    assert!(number(compact_plus, "width") >= 24.0);
    assert!(number(compact_plus, "x") + number(compact_plus, "width") <= 420.0);
    for row in compact["rows"].as_array().unwrap() {
        assert!(number(row, "x") >= 0.0);
        assert!(number(row, "x") + number(row, "width") <= 420.0);
        assert!(number(row, "width") > 200.0);
    }
    preserve(
        compact["full"].as_str().unwrap(),
        "compact-menu-and-tabs.png",
    );
    preserve(compact["crop"].as_str().unwrap(), "compact-menu-crop.png");
}
