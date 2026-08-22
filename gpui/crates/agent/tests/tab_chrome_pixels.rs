//! Production pixel and motion proof for the workspace tab chrome.

#![cfg(all(feature = "app", feature = "pixel"))]

mod support;

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
fn menu_field_edge_energy(image: &image::RgbaImage, bounds: &Value) -> f64 {
    let scale = image.width() as f64 / 1280.0;
    let x0 = ((number(bounds, "x") + 120.0) * scale).round() as u32;
    let x1 = ((number(bounds, "x") + number(bounds, "width") - 12.0) * scale).round() as u32;
    let y0 = ((number(bounds, "y") + 10.0) * scale).round() as u32;
    let y1 = ((number(bounds, "y") + number(bounds, "height") - 10.0) * scale).round() as u32;
    let luma = |x: u32, y: u32| {
        let pixel = image.get_pixel(x, y);
        (f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2])) / 3.0
    };
    let mut total = 0.0;
    let mut samples = 0_u64;
    for y in y0..y1 {
        for x in x0..x1 {
            if x + 1 < x1 {
                total += (luma(x + 1, y) - luma(x, y)).abs();
                samples += 1;
            }
            if y + 1 < y1 {
                total += (luma(x, y + 1) - luma(x, y)).abs();
                samples += 1;
            }
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
    std::env::set_var("LUMA_MOTION", "on");
    std::env::set_var("LUMA_MOTION_SCALE", "10");
    let mut harness = Fixture::new(
        "tab-chrome-pixels",
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0)],
    )
    .with_rig()
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
            const menuRows = ["Universe setup", "Pattern editor", "Track editor", "Visualizer"]
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
                s.find({ role: "button", label: "Universe setup" }) ? s : undefined);
            app.click(universeMenu.find({ role: "button", label: "Universe setup" }));
            until("the universe tab", (s) =>
                s.find({ role: "card", label: "Test Venue Universe setup" }));
            app.frames(2, { waitMs: 1100 });

            // Select the middle tab, then close through its real pointer
            // affordance so the successor hotspot is armed at that coordinate.
            // At this exact ownership threshold every tab chip is intentionally
            // clipped to a sliver. Select the already-open tab through the
            // product navigation seam; the assertion below is about the close
            // animation crossing strip owners, not pointer reachability here.
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
    let underlay_energy = menu_field_edge_energy(&pixels(&menu_underlay), menu);
    let animated_energy = menu_field_edge_energy(&pixels(&menu_animated_full), menu);
    let settled_energy = menu_field_edge_energy(&pixels(&menu_full), menu);
    println!(
        "menu field edge energy underlay={underlay_energy:.3} animated={animated_energy:.3} settled={settled_energy:.3}"
    );
    assert!(
        animated_energy < underlay_energy * 0.75,
        "animated popover did not materially blur/occlude its waveform: {animated_energy:.3} vs {underlay_energy:.3}"
    );
    assert!(
        settled_energy < underlay_energy * 0.75,
        "settled popover did not materially blur/occlude its waveform: {settled_energy:.3} vs {underlay_energy:.3}"
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
                s.find({ role: "button", label: "Universe setup" }) ? s : undefined);
            app.click(universeMenu.find({ role: "button", label: "Universe setup" }));
            until("compact Universe tab", (s) =>
                s.find({ role: "button", label: "Test Venue" }) ? s : undefined);
            // With the sidebar still consuming 275px, the 420px shell has no
            // legal workspace width beside CENTER_MIN. Hiding it starts the
            // real pane entrance; the add control must survive every frame as
            // ownership moves from the thread band to the workspace band.
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
                s.find({ role: "card", label: "Test Venue Universe setup" }));
            const compactPlusState = until("compact add-tab control", (s) =>
                s.find({ role: "button", label: "new-tab" }) ? s : undefined);
            const compactPlus = compactPlusState.find({ role: "button", label: "new-tab" });
            app.click(compactPlus);
            const shot = until("compact menu", (s) =>
                s.find({ role: "card", label: "New tab menu" }) ? s : undefined);
            const rows = ["Universe setup", "Pattern editor", "Track editor", "Visualizer"]
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

    // 650px starts the workspace just below the 24px ownership threshold.
    // Close in the thread-owned strip, then cross the boundary while the exit
    // is alive: only survivors re-anchor; exit and stable target stay fixed.
    let mut crossing = Fixture::new(
        "tab-chrome-pixels-crossing",
        20,
        vec![Clip::new("pattern-strobe", "Strobe", 2.0, 6.0)],
    )
    .with_rig()
    .window(650.0, 480.0)
    .open(Mode::Pixel);
    let crossing = run(
        &mut crossing,
        &support::script(
            r#"
            nav.trackEditor("Test Venue", "Aurora");
            nav.pattern("Strobe");
            const plusState = until("crossing add-tab", (s) =>
                s.find({ role: "button", label: "new-tab" }) ? s : undefined);
            app.click(plusState.find({ role: "button", label: "new-tab" }));
            const menu = until("crossing Universe choice", (s) =>
                s.find({ role: "button", label: "Universe setup" }) ? s : undefined);
            app.click(menu.find({ role: "button", label: "Universe setup" }));
            until("crossing Universe tab", (s) =>
                s.find({ role: "button", label: "Test Venue" }) ? s : undefined);
            app.click(app.snapshot().find({ role: "button", label: "Strobe" }));
            const closeState = until("crossing pointer close", (s) =>
                s.find({ role: "button", label: "Close Strobe" }) ? s : undefined);
            const close = closeState.find({ role: "button", label: "Close Strobe" });
            app.click(close, { restale: "match" });
            until("crossing close armed", (s) =>
                s.find((node) => node.role === "card" &&
                    node.label.startsWith("Closing Strobe opacity ")) &&
                s.find({ role: "button", label: "Close next tab" }) ? s : undefined);
            const before = app.screenshot().path;
            app.key("secondary-b");
            const exitXs = [];
            const hotspotXs = [];
            const neighborXs = [];
            const stripXs = [];
            for (let sample = 0; sample < 8; sample++) {
                const shot = app.snapshot();
                const exit = shot.find((node) =>
                    node.role === "card" && node.label.startsWith("Closing Strobe opacity "));
                const hotspot = shot.find({ role: "button", label: "Close next tab" });
                const neighbor = shot.findAll({ role: "button", label: "Test Venue" })
                    .sort((a, b) => b.bounds.x - a.bounds.x)[0];
                const strip = shot.find({ role: "card", label: "Tab strip" });
                if (!exit || !hotspot || !neighbor || !strip) {
                    throw new Error(`crossing transition node missing at sample ${sample}: ` +
                        JSON.stringify({ exit: !!exit, hotspot: !!hotspot,
                            neighbor: !!neighbor, strip: !!strip,
                            labels: shot.nodes.map((node) => `${node.role}:${node.label}`) }));
                }
                exitXs.push(exit.bounds.x);
                hotspotXs.push(hotspot.bounds.x);
                neighborXs.push(neighbor.bounds.x);
                stripXs.push(strip.bounds.x);
                app.frames(1, { waitMs: 80 });
            }
            const mid = app.screenshot().path;
            app.click(app.snapshot().find({ role: "button", label: "Close next tab" }),
                { restale: "match" });
            app.frames(2);
            ({
                before, mid, after: app.screenshot().path,
                exitXs, hotspotXs, neighborXs, stripXs,
                successorAfter: app.snapshot().find({ role: "button", label: "Close Test Venue" }),
            })
            "#,
        ),
    );
    let span = |values: &Value| {
        let values = values.as_array().unwrap();
        let low = values
            .iter()
            .map(|value| value.as_f64().unwrap())
            .fold(f64::INFINITY, f64::min);
        let high = values
            .iter()
            .map(|value| value.as_f64().unwrap())
            .fold(f64::NEG_INFINITY, f64::max);
        high - low
    };
    assert!(span(&crossing["exitXs"]) <= 1.0, "{crossing:#}");
    assert!(span(&crossing["hotspotXs"]) <= 1.0, "{crossing:#}");
    assert!(
        span(&crossing["stripXs"]) > 1.0,
        "ownership never crossed: {crossing:#}"
    );
    assert!(
        span(&crossing["neighborXs"]) > 1.0,
        "survivor did not re-anchor: {crossing:#}"
    );
    assert!(
        crossing["successorAfter"].is_null(),
        "same-coordinate close missed successor: {crossing:#}"
    );
    println!(
        "ownership crossing spans exit={:.3} hotspot={:.3} strip={:.3} neighbor={:.3}",
        span(&crossing["exitXs"]),
        span(&crossing["hotspotXs"]),
        span(&crossing["stripXs"]),
        span(&crossing["neighborXs"]),
    );
    preserve(
        crossing["before"].as_str().unwrap(),
        "close-ownership-before.png",
    );
    preserve(crossing["mid"].as_str().unwrap(), "close-ownership-mid.png");
    preserve(
        crossing["after"].as_str().unwrap(),
        "close-ownership-after.png",
    );
}
