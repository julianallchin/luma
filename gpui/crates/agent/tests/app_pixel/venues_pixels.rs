//! Pixel proof for every production venue-dialog route.

use super::support;
use gpui_agent::fixture::session;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{px, size, App, AppContext as _, Window};
use gpui_agent::shots::{differing_fraction, CHANNEL_NOISE};
use gpui_agent::{Config, Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;
use support::image::{edge_energy, preserve};

const LAST_VENUE: &str = "last-venue";

async fn seed(dir: &Path, remembered: bool) {
    let db = luma_lib::database::local::database::init_app_db_at(dir)
        .await
        .expect("failed to open pixel fixture app database");
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', ?, 'Pixel Venue')")
        .bind(session::PRINCIPAL)
        .execute(&db.0)
        .await
        .expect("failed to seed pixel venue");
    session::signed_in(dir).await;
    let state = luma_lib::database::local::state::init_state_db_at(dir)
        .await
        .expect("failed to open pixel fixture state database");
    if remembered {
        luma_lib::database::local::auth::set_session_item(&state.0, LAST_VENUE, "venue")
            .await
            .expect("failed to seed pixel preference");
    }
    db.0.close().await;
    state.0.close().await;
}

fn fixture_dir(name: &str, remembered: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "luma-gpui-venue-pixels-{name}-{}",
        std::process::id()
    ));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).expect("failed to create pixel fixture directory");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start pixel fixture runtime")
        .block_on(seed(&dir, remembered));
    dir
}

fn harness(dir: &Path, fixture: luma_app::NavigationFixture) -> Harness {
    let root: gpui_agent::RootFactory = Arc::new(move |window: &mut Window, cx: &mut App| {
        luma_app::init(cx);
        let mut library = luma_app::Library::open().expect("failed to open pixel library");
        library.set_navigation_fixture(fixture.clone());
        let luma = cx.new(|cx| luma_app::Luma::new(library, cx));
        cx.new(|cx| gpui_component::Root::new(luma, window, cx).bordered(false))
            .into()
    });
    Harness::headless(
        Config {
            mode: Mode::Pixel,
            window_size: size(px(900.0), px(650.0)),
            call_timeout: GPU_LIVENESS_TIMEOUT,
            runtime: support::runtime(dir),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start venue pixel harness")
}

fn luma_range(image: &image::RgbaImage) -> u8 {
    let (mut low, mut high) = (u8::MAX, u8::MIN);
    for pixel in image.pixels() {
        let luma = ((u16::from(pixel[0]) + u16::from(pixel[1]) + u16::from(pixel[2])) / 3) as u8;
        low = low.min(luma);
        high = high.max(luma);
    }
    high.saturating_sub(low)
}

fn number(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("missing numeric {key}: {value:#}"))
}

fn assert_card_geometry(label: &str, card: &Value) {
    let x = number(card, "x");
    let y = number(card, "y");
    let width = number(card, "width");
    let height = number(card, "height");
    assert!(x >= 15.0, "{label} escaped left gutter: {card:#}");
    assert!(y >= 37.0, "{label} overlapped titlebar: {card:#}");
    assert!(x + width <= 885.0, "{label} escaped right gutter: {card:#}");
    assert!(
        y + height <= 635.0,
        "{label} escaped bottom gutter: {card:#}"
    );
    assert!(
        width > 600.0 && height > 400.0,
        "{label} collapsed: {card:#}"
    );
}

#[test]
fn production_venue_routes_are_frosted_distinct_and_viewport_safe() {
    // A remembered venue opens straight onto its shell; the dialog's create
    // and join routes open over it from the title bar's venue menu.
    let restored_dir = fixture_dir("routes", true);
    let mut routes = harness(&restored_dir, luma_app::NavigationFixture::default());
    let shots = routes.run(
        &support::script(
            r#"
        until("the restored venue shell", (s) =>
            s.find({ role: "button", label: "Pixel Venue" }) !== undefined);
        const base = app.screenshot().path;
        const route = (row, ready) => {
            nav.venueMenu();
            nav.step(row, "row", row);
            const shot = until(`the ${row} route`, ready);
            return { shot, card: shot.find({ role: "card", label: "Venue dialog" }) };
        };
        const create = route("New venue…", (s) =>
            s.find({ role: "input", label: "Venue name" })?.focused === true ? s : undefined);
        const createShot = app.screenshot().path;
        const createCardShot = app.screenshot({ node: create.card }).path;
        app.key("escape");
        until("the dialog gone", (s) => s.find({ role: "card", label: "Venue dialog" }) === undefined);
        const join = route("Join with code…", (s) =>
            s.find({ role: "text", label: "Join a venue" }) !== undefined ? s : undefined);
        ({ base,
           create: createShot,
           createCardShot,
           createBounds: create.card.bounds,
           join: app.screenshot().path,
           joinCardShot: app.screenshot({ node: join.card }).path,
           joinBounds: join.card.bounds })
        "#,
        ),
        GPU_LIVENESS_TIMEOUT,
    );
    drop(routes);

    // With no venue remembered the picker is the window: its loading route,
    // then the list.
    let launch_dir = fixture_dir("launch", false);
    let mut launch = harness(
        &launch_dir,
        luma_app::NavigationFixture {
            // The catalogue read is held, so the loading route is on screen.
            catalogue_responses: vec![(Duration::from_millis(1_500), None)],
            ..Default::default()
        },
    );
    let launch_shots = launch.run(
        &support::script(
            r#"
        let shot = until("the loading route", (s) =>
            s.find({ role: "text", label: "Loading venues…" }) !== undefined);
        const loadingCard = shot.find({ role: "card", label: "Venue dialog" });
        const loading = app.screenshot().path;
        const loadingCardShot = app.screenshot({ node: loadingCard }).path;
        shot = until("the browse route", (s) =>
            s.find({ role: "input", label: "Search venues…" })?.focused === true
                && s.find({ role: "card", label: "Pixel Venue" }) !== undefined);
        const browseCard = shot.find({ role: "card", label: "Venue dialog" });
        ({ loading,
           loadingCardShot,
           loadingBounds: loadingCard.bounds,
           browse: app.screenshot().path,
           browseCardShot: app.screenshot({ node: browseCard }).path,
           browseBounds: browseCard.bounds })
        "#,
        ),
        GPU_LIVENESS_TIMEOUT,
    );
    drop(launch);

    let error_dir = fixture_dir("error", false);
    let long_error = format!(
        "Pixel catalogue failure while reading venue metadata.\n{}\n{}",
        "UNBROKEN_ERROR_TOKEN_".repeat(180),
        (0..80)
            .map(|line| format!("diagnostic line {line}: the venue catalogue remains unavailable"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut error = harness(
        &error_dir,
        luma_app::NavigationFixture {
            // The launch picker's read is the one that fails.
            catalogue_responses: vec![(Duration::from_millis(1_500), Some(long_error))],
            ..Default::default()
        },
    );
    let error_shots = error.run(
        &support::script(
            r#"
        until("the loading route", (s) =>
            s.find({ role: "text", label: "Loading venues…" }) !== undefined);
        const errorLoading = app.screenshot().path;
        const shot = until("the pixel error route", (s) =>
            s.find({ role: "text", label: "Could not load your venues" }) !== undefined);
        const card = shot.find({ role: "card", label: "Venue dialog" });
        const errorViewport = shot.find({ role: "card", label: "Venue error viewport" });
        const errorText = shot.find({ role: "text", label: "Could not load your venues" });
        // The backend's text is for the log: none of it reaches the dialog.
        const leaked = shot.nodes.some((n) => (n.label || "").includes("Pixel catalogue failure")
            || (n.label || "").includes("UNBROKEN_ERROR_TOKEN_"));
        ({ errorLoading,
           error: app.screenshot().path,
           errorCardShot: app.screenshot({ node: card }).path,
           errorBounds: card.bounds,
           errorViewportBounds: errorViewport.bounds,
           errorTextBounds: errorText.bounds,
           leaked })
        "#,
        ),
        GPU_LIVENESS_TIMEOUT,
    );

    for (label, bounds) in [
        ("loading", &launch_shots["loadingBounds"]),
        ("browse", &launch_shots["browseBounds"]),
        ("join", &shots["joinBounds"]),
        ("create", &shots["createBounds"]),
        ("error", &error_shots["errorBounds"]),
    ] {
        assert_card_geometry(label, bounds);
    }
    let error_card = &error_shots["errorBounds"];
    let error_viewport = &error_shots["errorViewportBounds"];
    let error_text = &error_shots["errorTextBounds"];
    let viewport_x = number(error_viewport, "x");
    let viewport_y = number(error_viewport, "y");
    let viewport_width = number(error_viewport, "width");
    let viewport_height = number(error_viewport, "height");
    let text_x = number(error_text, "x");
    let text_y = number(error_text, "y");
    let text_width = number(error_text, "width");
    let text_height = number(error_text, "height");
    let card_x = number(error_card, "x");
    let card_y = number(error_card, "y");
    let card_width = number(error_card, "width");
    let card_height = number(error_card, "height");
    assert!(
        viewport_x >= card_x + 32.0
            && viewport_x + viewport_width <= card_x + card_width - 32.0,
        "error viewport escaped the dialog's horizontal content gutters: viewport={error_viewport:#}, card={error_card:#}"
    );
    assert!(
        viewport_y >= card_y && viewport_y + viewport_height <= card_y + card_height,
        "error viewport escaped the dialog vertically: viewport={error_viewport:#}, card={error_card:#}"
    );
    assert!(
        text_x >= viewport_x
            && text_x + text_width <= viewport_x + viewport_width
            && text_y >= viewport_y
            && text_y + text_height <= viewport_y + viewport_height,
        "ellipsized error text escaped its clip viewport: text={error_text:#}, viewport={error_viewport:#}"
    );
    assert_eq!(
        error_shots["leaked"],
        serde_json::Value::Bool(false),
        "the backend's adversarial error text reached the venue dialog"
    );
    println!(
        "venue error geometry: card=({card_x:.0},{card_y:.0}) {card_width:.0}x{card_height:.0}; viewport=({viewport_x:.0},{viewport_y:.0}) {viewport_width:.0}x{viewport_height:.0}; text=({text_x:.0},{text_y:.0}) {text_width:.0}x{text_height:.0}"
    );

    let loading = preserve(
        launch_shots["loading"].as_str().unwrap(),
        "venues/venue-loading",
    );
    let base = preserve(shots["base"].as_str().unwrap(), "venues/venue-shell-base");
    let browse = preserve(
        launch_shots["browse"].as_str().unwrap(),
        "venues/venue-browse",
    );
    let create = preserve(shots["create"].as_str().unwrap(), "venues/venue-create");
    let join = preserve(shots["join"].as_str().unwrap(), "venues/venue-join");
    let error_loading = preserve(
        error_shots["errorLoading"].as_str().unwrap(),
        "venues/venue-error-loading",
    );
    let error = preserve(error_shots["error"].as_str().unwrap(), "venues/venue-error");
    let card_paths = [
        preserve(
            launch_shots["loadingCardShot"].as_str().unwrap(),
            "venues/venue-loading-card",
        ),
        preserve(
            launch_shots["browseCardShot"].as_str().unwrap(),
            "venues/venue-browse-card",
        ),
        preserve(
            shots["createCardShot"].as_str().unwrap(),
            "venues/venue-create-card",
        ),
        preserve(
            shots["joinCardShot"].as_str().unwrap(),
            "venues/venue-join-card",
        ),
        preserve(
            error_shots["errorCardShot"].as_str().unwrap(),
            "venues/venue-error-card",
        ),
    ];

    let loading_pixels = support::image::decode(&loading);
    let base_pixels = support::image::decode(&base);
    let browse_pixels = support::image::decode(&browse);
    let join_pixels = support::image::decode(&join);
    let create_pixels = support::image::decode(&create);
    let error_loading_pixels = support::image::decode(&error_loading);
    let error_pixels = support::image::decode(&error);
    let shell_edge_before = edge_energy(&base_pixels, 300..850, 2..34);
    let shell_edge_after = edge_energy(&create_pixels, 300..850, 2..34);
    let agent_edge_before = edge_energy(&base_pixels, 8..250, 2..34);
    let agent_edge_after = edge_energy(&create_pixels, 8..250, 2..34);
    let shell_change = differing_fraction(&base_pixels, &create_pixels, CHANNEL_NOISE);
    let join_create_change = differing_fraction(&join_pixels, &create_pixels, CHANNEL_NOISE);
    let error_change = differing_fraction(&error_loading_pixels, &error_pixels, CHANNEL_NOISE);
    let browse_change = differing_fraction(&loading_pixels, &browse_pixels, CHANNEL_NOISE);
    println!(
        "venue pixels: shell changed={shell_change:.3}, join/create={join_create_change:.3}, error transition={error_change:.3}, loading/browse={browse_change:.3}; titlebar edge shell {shell_edge_before:.3}->{shell_edge_after:.3}, agent panel {agent_edge_before:.3}->{agent_edge_after:.3}"
    );
    assert!(
        shell_change > 0.20,
        "venue modal did not visibly scrim/blur the production shell: {shell_change:.3}"
    );
    assert!(
        join_create_change > 0.02,
        "join and create rendered as the same route: {join_create_change:.3}"
    );
    assert!(
        error_change > 0.005,
        "loading and error rendered as the same route: {error_change:.3}"
    );
    assert!(
        browse_change > 0.005,
        "loading and browse rendered as the same route: {browse_change:.3}"
    );
    assert!(
        shell_edge_after < shell_edge_before,
        "shell backdrop did not reduce titlebar edge energy: {shell_edge_before:.3}->{shell_edge_after:.3}"
    );
    assert!(
        agent_edge_after < agent_edge_before,
        "agent panel backdrop did not reduce titlebar edge energy: {agent_edge_before:.3}->{agent_edge_after:.3}"
    );
    assert!(
        differing_fraction(&loading_pixels, &error_loading_pixels, CHANNEL_NOISE) < 0.02,
        "loading paint changed substantially between route outcomes"
    );
    for path in card_paths {
        assert!(
            luma_range(&support::image::decode(path)) > 24,
            "venue route card is a flat placeholder"
        );
    }
}
