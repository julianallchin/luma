//! Venue launch, selection and onboarding through the production app tree.

#![cfg(feature = "app")]

use super::support;
use support::session;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;

const LAST_VENUE: &str = "last-venue";

async fn seed(dir: &Path, venues: &[(&str, &str)], remembered: Option<&str>, tracks: bool) {
    let db = luma_lib::database::local::database::init_app_db_at(dir)
        .await
        .expect("failed to open fixture app database");
    for (id, name) in venues {
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES (?, ?, ?)")
            .bind(id)
            .bind(session::PRINCIPAL)
            .bind(name)
            .execute(&db.0)
            .await
            .expect("failed to seed venue");
        if tracks {
            let track_id = format!("track-{id}");
            sqlx::query(
                "INSERT INTO tracks
                    (id, uid, track_hash, title, artist, duration_seconds, file_path)
                 VALUES (?, ?, ?, ?, 'Fixture', 60.0, ?)",
            )
            .bind(&track_id)
            .bind(session::PRINCIPAL)
            .bind(format!("hash-{id}"))
            .bind(format!("{name} Track"))
            .bind(format!("/fixture/{id}.wav"))
            .execute(&db.0)
            .await
            .expect("failed to seed track");
            sqlx::query(
                "INSERT INTO scores (id, uid, track_id, venue_id, name)
                 VALUES (?, ?, ?, ?, 'Fixture Score')",
            )
            .bind(format!("score-{id}"))
            .bind(session::PRINCIPAL)
            .bind(&track_id)
            .bind(id)
            .execute(&db.0)
            .await
            .expect("failed to seed venue score");
        }
    }
    session::signed_in(dir).await;
    let state = luma_lib::database::local::state::init_state_db_at(dir)
        .await
        .expect("failed to open fixture state database");
    if let Some(remembered) = remembered {
        luma_lib::database::local::auth::set_session_item(&state.0, LAST_VENUE, remembered)
            .await
            .expect("failed to seed remembered venue");
    }
    db.0.close().await;
    state.0.close().await;
}

fn fixture_dir(
    name: &str,
    venues: &[(&str, &str)],
    remembered: Option<&str>,
    tracks: bool,
) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-venues-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("failed to create fixture directory");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start fixture runtime")
        .block_on(seed(&dir, venues, remembered, tracks));
    dir
}

fn session_item(dir: &Path) -> Option<String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start session reader")
        .block_on(async {
            let state = luma_lib::database::local::state::init_state_db_at(dir)
                .await
                .expect("failed to reopen fixture state database");
            let value = luma_lib::database::local::auth::get_session_item(&state.0, LAST_VENUE)
                .await
                .expect("failed to read remembered venue");
            state.0.close().await;
            value
        })
}

fn harness(dir: &Path, fixture: luma_app::NavigationFixture) -> Harness {
    let root: gpui_agent::RootFactory =
        Arc::new(move |window: &mut Window, cx: &mut App| -> AnyView {
            luma_app::init(cx);
            let mut library = luma_app::Library::open().expect("failed to open fixture library");
            library.set_navigation_fixture(fixture.clone());
            let luma = cx.new(|cx| luma_app::Luma::new(library, cx));
            cx.new(|cx| gpui_component::Root::new(luma, window, cx).bordered(false))
                .into()
        });
    Harness::headless(
        Config {
            mode: Mode::Headless,
            call_timeout: GPU_LIVENESS_TIMEOUT,
            runtime: support::runtime(dir),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start harness")
}

fn exec(harness: &mut Harness, script: &str) -> Value {
    let result = harness.exec(&support::script(script), Duration::from_secs(90));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

#[test]
fn venue_launch_picker_create_and_stale_reads_are_correlated() {
    // No preference: first paint is loading, every venue is searchable, and a
    // deliberately late Alpha track read cannot overwrite Beta's browser.
    let browse_dir = fixture_dir(
        "browse",
        &[("alpha", "Alpha Hall"), ("beta", "Beta Room")],
        None,
        true,
    );
    let mut browse = harness(
        &browse_dir,
        luma_app::NavigationFixture {
            track_delays: HashMap::new(),
            catalogue_responses: vec![
                (Duration::from_millis(180), None),
                (Duration::ZERO, None),
                (Duration::ZERO, None),
                (
                    Duration::from_millis(400),
                    Some("stale catalogue failure".to_string()),
                ),
                (Duration::from_millis(20), None),
            ],
            track_responses: vec![
                ("beta".into(), Duration::ZERO, Some("Beta initial".into())),
                (
                    "alpha".into(),
                    Duration::from_millis(900),
                    Some("Alpha stale".into()),
                ),
                ("beta".into(), Duration::ZERO, Some("Beta current".into())),
                (
                    "beta".into(),
                    Duration::from_millis(400),
                    Some("Beta same-id stale".into()),
                ),
                (
                    "beta".into(),
                    Duration::from_millis(20),
                    Some("Beta same-id latest".into()),
                ),
            ],
            session_write_delays: vec![Duration::ZERO, Duration::from_millis(350), Duration::ZERO],
            ..Default::default()
        },
    );
    let out = exec(
        &mut browse,
        r#"
        const initial = app.snapshot();
        const loading = initial.find({ role: "text", label: "Loading venues…" }) !== undefined;
        const ready = until("the venue catalogue", (s) =>
            s.find({ role: "card", label: "Alpha Hall" }) !== undefined
                && s.find({ role: "card", label: "Beta Room" }) !== undefined);
        const search = ready.find({ role: "input", label: "Search venues…" });
        app.type(search, "beta");
        const filtered = until("the filtered venue", (s) =>
            s.find({ role: "card", label: "Beta Room" }) !== undefined
                && s.find({ role: "card", label: "Alpha Hall" }) === undefined);
        app.click(filtered.find({ role: "card", label: "Beta Room" }));
        const beta = until("Beta's browser", (s) =>
            s.find({ role: "button", label: "Beta Room" }) !== undefined
                && s.find({ role: "row", label: "Beta initial" }) !== undefined);

        // Start Alpha, then leave it before its delayed result lands.
        app.click(beta.find({ role: "button", label: "Beta Room" }));
        let picker = until("the reopened venue picker", (s) =>
            s.find({ role: "card", label: "Alpha Hall" }) !== undefined);
        app.click(picker.find({ role: "card", label: "Alpha Hall" }));
        let alpha = until("Alpha's loading browser", (s) =>
            s.find({ role: "button", label: "Alpha Hall" }) !== undefined);
        app.click(alpha.find({ role: "button", label: "Alpha Hall" }));
        picker = until("the picker over Alpha", (s) =>
            s.find({ role: "card", label: "Beta Room" }) !== undefined);
        app.click(picker.find({ role: "card", label: "Beta Room" }));
        until("Beta after the replacement", (s) =>
            s.find({ role: "row", label: "Beta current" }) !== undefined);
        app.frames(20, { waitMs: 60 });
        let settled = app.snapshot();

        // A dismissed slow catalogue request cannot overwrite the next picker.
        app.click(settled.find({ role: "button", label: "Beta Room" }));
        const staleLoading = until("the stale catalogue loading route", (s) =>
            s.find({ role: "text", label: "Loading venues…" }) !== undefined);
        app.action("luma::DismissOverlay");
        const returnedBetween = until("focus returned between catalogue requests", (s) =>
            s.find({ role: "button", label: "Beta Room" })?.focused === true);
        app.click(returnedBetween.find({ role: "button", label: "Beta Room" }));
        const dialog = until("the current venue picker", (s) =>
            s.find({ role: "input", label: "Search venues…" })?.focused === true
                && s.find({ role: "card", label: "Beta Room" }) !== undefined);
        app.frames(10, { waitMs: 50 });
        const afterStaleCatalogue = app.snapshot();

        // Name the complete venue-dialog ring. Every focused leaf must be one
        // of these controls and geometrically inside the dialog card; the
        // sidebar opener is deliberately not a member while the modal exists.
        const focusOrder = [
            "input:Search venues…",
            // Patterns left this dialog: picking a room and picking a light
            // pattern are different questions, and only one of them is what
            // the venue switcher is for.
            "button:Create venue",
            // The palette's header carries its own dismissal cap, so the ring
            // now passes through it on the way to the list.
            "button:Close",
            "card:Alpha Hall",
            "card:Beta Room",
        ];
        const focusable = new Set(focusOrder);
        const dialogBounds = dialog.find({ role: "card", label: "Venue dialog" }).bounds;
        const contained = (outer, inner) =>
            inner.x >= outer.x - 0.5 && inner.y >= outer.y - 0.5
                && inner.x + inner.width <= outer.x + outer.width + 0.5
                && inner.y + inner.height <= outer.y + outer.height + 0.5;
        const focusedLeaf = (frame) => {
            if (frame.find({ role: "button", label: "Beta Room" })?.focused === true) {
                throw new Error("the sidebar opener received focus through the modal");
            }
            const leaves = frame.findAll((node) => node.focused)
                .filter((node) => !(node.role === "card" && node.label === "Venue dialog"));
            if (leaves.length !== 1) {
                throw new Error(`expected one dialog focus leaf, got ${leaves.map((node) => `${node.role}:${node.label}`)}`);
            }
            const leaf = leaves[0];
            const identity = `${leaf.role}:${leaf.label}`;
            if (!focusable.has(identity)) {
                throw new Error(`focus escaped the explicit venue allowlist to ${identity}`);
            }
            if (!contained(dialogBounds, leaf.bounds)) {
                throw new Error(`focused ${identity} escaped the venue dialog bounds`);
            }
            return identity;
        };
        const focusTrace = [focusedLeaf(dialog)];
        app.key("shift-tab");
        app.frames(2);
        const reverseLast = focusedLeaf(app.snapshot());
        focusTrace.push(reverseLast);
        app.key("tab");
        app.frames(2);
        const reverseFirst = focusedLeaf(app.snapshot());
        focusTrace.push(reverseFirst);
        const forwardLeaves = [];
        for (const expected of focusOrder.slice(1)) {
            app.key("tab");
            app.frames(2);
            const actual = focusedLeaf(app.snapshot());
            forwardLeaves.push(actual);
            if (actual !== expected) {
                throw new Error(`venue focus order expected ${expected}, got ${actual}`);
            }
        }
        app.key("tab");
        app.frames(2);
        const forwardFirst = focusedLeaf(app.snapshot());
        focusTrace.push(...forwardLeaves, forwardFirst);
        // Coordinate activation of the shell control behind the modal lands
        // on the optional scrim: the venue dialog dismisses, but the covered
        // account action must not run.
        const occludedAccount = app.snapshot().find({ role: "button", label: "Account" });
        app.click(occludedAccount);
        app.frames(2);
        const afterOccludedClick = app.snapshot();
        app.key("escape");
        const restoredOpener = until("the exact venue opener after Escape", (s) =>
            s.find({ role: "button", label: "Beta Room" })?.focused === true);

        // Two reads of the same venue: the second answer must win even though
        // the first lands later.
        app.click(restoredOpener.find({ role: "button", label: "Beta Room" }));
        picker = until("the same-id stale picker", (s) =>
            s.find({ role: "card", label: "Beta Room" }) !== undefined);
        app.click(picker.find({ role: "card", label: "Beta Room" }));
        let same = until("the same-id stale load", (s) =>
            s.find({ role: "button", label: "Beta Room" }) !== undefined);
        app.click(same.find({ role: "button", label: "Beta Room" }));
        picker = until("the same-id latest picker", (s) =>
            s.find({ role: "card", label: "Beta Room" }) !== undefined);
        app.click(picker.find({ role: "card", label: "Beta Room" }));
        until("the same-id latest row", (s) =>
            s.find({ role: "row", label: "Beta same-id latest" }) !== undefined);
        app.frames(10, { waitMs: 50 });
        settled = app.snapshot();
        ({ loading,
           searched: filtered.findAll({ role: "card" })
               .map((n) => n.label).filter((label) => label === "Beta Room"),
           staleSafe: settled.find({ role: "button", label: "Beta Room" }) !== undefined
               && settled.find({ role: "row", label: "Beta same-id latest" }) !== undefined
               && settled.find({ role: "row", label: "Beta same-id stale" }) === undefined
               && settled.find({ role: "row", label: "Alpha stale" }) === undefined,
           staleCatalogueRejected:
               afterStaleCatalogue.find((n) => n.role === "text" && n.label.includes("stale catalogue failure")) === undefined,
           returnedBetween: returnedBetween.find({ role: "button", label: "Beta Room" })?.focused === true,
           focusContract: focusTrace[0] === focusOrder[0]
               && reverseLast === focusOrder[focusOrder.length - 1]
               && reverseFirst === focusOrder[0]
               && forwardFirst === focusOrder[0],
           focusTrace,
           occluded: afterOccludedClick.find({ role: "card", label: "Venue dialog" }) === undefined
               && afterOccludedClick.find({ role: "row", label: "Settings" }) === undefined,
           restoredOpener: restoredOpener.find({ role: "button", label: "Beta Room" })?.focused === true })
    "#,
    );
    assert_eq!(out["loading"], true);
    assert_eq!(out["searched"], serde_json::json!(["Beta Room"]));
    assert_eq!(out["staleSafe"], true);
    assert_eq!(
        out["focusTrace"],
        serde_json::json!([
            "input:Search venues…",
            // shift-tab from the first stop wraps to the last…
            "card:Beta Room",
            "input:Search venues…",
            // Patterns left this dialog: picking a room and picking a light
            // pattern are different questions, and only one of them is what
            // the venue switcher is for.
            "button:Create venue",
            "button:Close",
            "card:Alpha Hall",
            "card:Beta Room",
            "input:Search venues…"
        ])
    );
    for key in [
        "staleCatalogueRejected",
        "returnedBetween",
        "focusContract",
        "occluded",
        "restoredOpener",
    ] {
        assert_eq!(
            out[key], true,
            "venue focus/race contract failed at {key}: {out:#}"
        );
    }
    drop(browse);

    // The final selection is durable and launches directly on a fresh app.
    let mut restored = harness(
        &browse_dir,
        luma_app::NavigationFixture {
            venues_delay: Duration::from_millis(180),
            ..Default::default()
        },
    );
    let out = exec(
        &mut restored,
        r#"
        const loading = app.snapshot().find({ role: "text", label: "Loading venues…" }) !== undefined;
        const shot = until("the restored Beta venue", (s) =>
            s.find({ role: "button", label: "Beta Room" }) !== undefined
                && s.find({ role: "row", label: "Beta Room Track" }) !== undefined);
        ({ loading,
           restored: shot.find({ role: "card", label: "Beta Room" }) === undefined,
           track: shot.find({ role: "row", label: "Beta Room Track" }) !== undefined })
    "#,
    );
    assert_eq!(out["loading"], true);
    assert_eq!(out["restored"], true);
    assert_eq!(out["track"], true);
    drop(restored);

    // A deleted preference falls back to the picker and is not mistaken for
    // an empty catalogue.
    let stale_dir = fixture_dir(
        "stale",
        &[("present", "Present Venue")],
        Some("deleted-venue"),
        false,
    );
    let mut stale = harness(
        &stale_dir,
        luma_app::NavigationFixture {
            venues_delay: Duration::from_millis(180),
            ..Default::default()
        },
    );
    let out = exec(
        &mut stale,
        r#"
        const loading = app.snapshot().find({ role: "text", label: "Loading venues…" }) !== undefined;
        const shot = until("the stale preference fallback", (s) =>
            s.find({ role: "card", label: "Present Venue" }) !== undefined);
        app.frames(4, { waitMs: 50 });
        ({ loading,
           picker: shot.find({ role: "input", label: "Search venues…" }) !== undefined,
           onboarding: shot.find({ role: "text", label: "Create your first venue" }) === undefined })
    "#,
    );
    assert_eq!(out["loading"], true);
    assert_eq!(out["picker"], true);
    assert_eq!(out["onboarding"], true);
    drop(stale);
    assert_eq!(session_item(&stale_dir), None);

    // Zero venues is a focused create state. Submission goes through the real
    // create_venue command, opens the result, and the next launch restores it.
    let empty_dir = fixture_dir("empty", &[], None, false);
    let mut empty = harness(
        &empty_dir,
        luma_app::NavigationFixture {
            venues_delay: Duration::from_millis(180),
            ..Default::default()
        },
    );
    let out = exec(
        &mut empty,
        r#"
        const loading = app.snapshot().find({ role: "text", label: "Loading venues…" }) !== undefined;
        const create = until("first venue onboarding", (s) =>
            s.find({ role: "input", label: "Venue name" })?.focused === true);
        const input = create.find({ role: "input", label: "Venue name" });
        app.type(input, "Morning Room");
        let named = until("the venue name", (s) =>
            s.find({ role: "input", label: "Morning Room" }) !== undefined);
        app.click(named.find({ role: "button", label: "Create venue" }));
        until("the created venue", (s) =>
            s.find({ role: "button", label: "Morning Room" }) !== undefined);
        app.frames(4, { waitMs: 50 });
        // The venue opening and its (empty) track list arriving are two
        // different frames — read the list after both have landed.
        const opened = until("the new venue's empty track list", (s) =>
            s.find({ role: "text", label: "No tracks imported" }) !== undefined ? s : undefined);
        ({ loading,
           focused: create.find({ role: "input", label: "Venue name" }).focused,
           opened: opened.find({ role: "text", label: "No tracks imported" }) !== undefined })
    "#,
    );
    assert_eq!(out["loading"], true);
    assert_eq!(out["focused"], true);
    assert_eq!(out["opened"], true);
    drop(empty);
    let mut created_restore = harness(&empty_dir, luma_app::NavigationFixture::default());
    let out = exec(
        &mut created_restore,
        r#"
        const shot = until("the created venue restore", (s) =>
            s.find({ role: "button", label: "Morning Room" }) !== undefined);
        shot.find({ role: "card", label: "Morning Room" }) === undefined
    "#,
    );
    assert_eq!(out, Value::Bool(true));
    drop(created_restore);

    // Session preference writes are serialized, not merely spawned. The slow
    // first choice must finish before the fast second choice, leaving the
    // user's final selection durable after both tasks have settled.
    let fifo_dir = fixture_dir(
        "fifo",
        &[("alpha", "Alpha Hall"), ("beta", "Beta Room")],
        None,
        false,
    );
    let mut fifo = harness(
        &fifo_dir,
        luma_app::NavigationFixture {
            session_write_delays: vec![Duration::from_millis(350), Duration::ZERO],
            ..Default::default()
        },
    );
    let out = exec(
        &mut fifo,
        r#"
        let picker = until("the FIFO venue picker", (s) =>
            s.find({ role: "card", label: "Alpha Hall" }) !== undefined);
        app.click(picker.find({ role: "card", label: "Alpha Hall" }));
        const alpha = until("the FIFO Alpha opener", (s) =>
            s.find({ role: "button", label: "Alpha Hall" }) !== undefined);
        app.click(alpha.find({ role: "button", label: "Alpha Hall" }));
        picker = until("the FIFO replacement picker", (s) =>
            s.find({ role: "card", label: "Beta Room" }) !== undefined);
        app.click(picker.find({ role: "card", label: "Beta Room" }));
        const beta = until("the FIFO Beta opener", (s) =>
            s.find({ role: "button", label: "Beta Room" }) !== undefined);
        app.frames(12, { waitMs: 50 });
        beta.find({ role: "button", label: "Beta Room" }) !== undefined
        "#,
    );
    assert_eq!(out, Value::Bool(true));
    drop(fifo);
    assert_eq!(session_item(&fifo_dir).as_deref(), Some("beta"));
    let mut fifo_restore = harness(
        &fifo_dir,
        luma_app::NavigationFixture {
            venues_delay: Duration::from_millis(180),
            ..Default::default()
        },
    );
    let out = exec(
        &mut fifo_restore,
        r#"
        const loading = app.snapshot().find({ role: "text", label: "Loading venues…" }) !== undefined;
        const shot = until("the FIFO final selection restore", (s) =>
            s.find({ role: "button", label: "Beta Room" }) !== undefined);
        ({ loading,
           beta: shot.find({ role: "button", label: "Beta Room" }) !== undefined,
           picker: shot.find({ role: "card", label: "Beta Room" }) !== undefined })
        "#,
    );
    assert_eq!(out["loading"], true);
    assert_eq!(out["beta"], true);
    assert_eq!(out["picker"], false);
    drop(fifo_restore);

    // Failure is its own operable route, after a visible loading frame.
    let error_dir = fixture_dir("error", &[("venue", "Venue")], None, false);
    let mut failed = harness(
        &error_dir,
        luma_app::NavigationFixture {
            venues_delay: Duration::from_millis(180),
            venues_error: Some("fixture catalogue failure".to_string()),
            track_delays: HashMap::new(),
            ..Default::default()
        },
    );
    let out = exec(
        &mut failed,
        r#"
        const loading = app.snapshot().find({ role: "text", label: "Loading venues…" }) !== undefined;
        const shot = until("the venue error", (s) =>
            s.find((n) => n.role === "text" && n.label.includes("fixture catalogue failure")) !== undefined);
        ({ loading,
           error: shot.find((n) => n.role === "text" && n.label.includes("fixture catalogue failure")) !== undefined,
           retry: shot.find({ role: "button", label: "Retry" }) !== undefined,
           empty: shot.find({ role: "text", label: "Create your first venue" }) !== undefined })
    "#,
    );
    assert_eq!(out["loading"], true);
    assert_eq!(out["error"], true);
    assert_eq!(out["retry"], true);
    assert_eq!(out["empty"], false);
}

/// Leaving a room revokes its track as a *subject* without throwing the work
/// away.
///
/// Both halves matter and they pull in opposite directions: whichever surface
/// is offering new tabs must stop offering a track the current browser cannot
/// honestly open, while the tabs that were open under that track have to
/// survive — parked under it, and back on screen the moment it is picked
/// again. Asserting only the first half would pass a shell that closed those
/// tabs outright.
#[test]
fn switching_venues_parks_the_track_subject_and_revokes_it_from_the_new_tab_menu() {
    let dir = fixture_dir(
        "tab-subject",
        &[("alpha", "Alpha Hall"), ("beta", "Beta Room")],
        None,
        true,
    );
    let mut app = harness(&dir, luma_app::NavigationFixture::default());
    let out = exec(
        &mut app,
        r#"
        let picker = until("the two venues", (s) =>
            s.find({ role: "card", label: "Alpha Hall" }) !== undefined
                && s.find({ role: "card", label: "Beta Room" }) !== undefined);
        app.click(picker.find({ role: "card", label: "Alpha Hall" }));
        until("Alpha's track", (s) =>
            s.find({ role: "row", label: "Alpha Hall Track" }) !== undefined ? s : undefined);
        // Two gestures now: the row goes to the track's scores, and one of
        // them is the timeline. `nav.track` is that walk, and it comes back
        // out to the list.
        nav.track("Alpha Hall Track");
        until("the selected Alpha tab", (s) =>
            s.find({ role: "button", label: "Alpha Hall Track" }) !== undefined);

        app.click(app.snapshot().find({ role: "button", label: "Alpha Hall" }));
        picker = until("the venue switcher", (s) =>
            s.find({ role: "card", label: "Beta Room" }) !== undefined ? s : undefined);
        app.click(picker.find({ role: "card", label: "Beta Room" }));
        until("Beta's track", (s) =>
            s.find({ role: "row", label: "Beta Room Track" }) !== undefined);
        // Beta has nothing open, so the offer is the panel's empty state
        // rather than the `+` menu — same three choices, same reasons, and
        // with no tabs there is no `+` for them to hang off.
        const offer = until("the empty panel's offer", (s) =>
            s.find({ role: "card", label: "Empty panel" }) !== undefined ? s : undefined);
        const track = offer.find({ role: "button", label: "Track editor" });
        const reason = offer.find({ role: "text", label: "Select a track first" }) !== undefined;
        // The strip belongs to the picked track, so leaving Alpha parks its
        // tabs instead of keeping them on screen beside Beta's.
        const oldTabParked = offer.find({ role: "button", label: "Alpha Hall Track" }) === undefined;

        // Parked is not closed: go back and re-pick the track, and the set
        // that was open under it comes back with it.
        // Polled clicks from here: swapping the strip is itself a re-render, so
        // a node found in a snapshot taken before it lands is a stale frame.
        nav.step("the venue switcher", "button", "Beta Room");
        nav.venue("Alpha Hall");
        until("Alpha's track again", (s) =>
            s.find({ role: "row", label: "Alpha Hall Track" }) !== undefined);
        // Its tabs are parked under the *track*, not the room, so landing back
        // in Alpha is not yet enough to bring them back.
        const parkedUntilRepicked =
            app.snapshot().find({ role: "button", label: "Alpha Hall Track" }) === undefined;
        nav.track("Alpha Hall Track");
        const restored = until("the restored Alpha tab", (s) =>
            s.find({ role: "button", label: "Alpha Hall Track" }) !== undefined ? s : undefined);
        ({
            enabled: track.enabled,
            reason,
            oldTabParked,
            parkedUntilRepicked,
            restored: restored.find({ role: "button", label: "Alpha Hall Track" }) !== undefined,
        })
        "#,
    );
    assert_eq!(
        out["enabled"], false,
        "stale track stayed actionable: {out:#}"
    );
    assert_eq!(
        out["reason"], true,
        "stale track had no honest reason: {out:#}"
    );
    assert_eq!(
        out["oldTabParked"], true,
        "another room's track tab is still in the strip: {out:#}"
    );
    assert_eq!(
        out["parkedUntilRepicked"], true,
        "returning to the room restored a tab before its track was picked: {out:#}"
    );
    assert_eq!(
        out["restored"], true,
        "venue switch destroyed an open track tab: re-picking it did not bring \
         its tabs back, so the work was thrown away rather than parked: {out:#}"
    );
}
