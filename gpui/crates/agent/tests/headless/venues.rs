//! Venue launch, selection and onboarding through the production app tree.
//!
//! These stay in Rust: they inject catalogue and track read delays
//! (`NavigationFixture`), relaunch the app on one library, and read the
//! remembered venue from the state database. The venue-switch test that needs
//! none of that is `tests/js/headless/venues.test.js`.

use super::support;
use gpui_agent::fixture::session;

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

fn session_item(dir: &Path, key: &str) -> Option<String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start session reader")
        .block_on(async {
            let state = luma_lib::database::local::state::init_state_db_at(dir)
                .await
                .expect("failed to reopen fixture state database");
            let value = luma_lib::database::local::auth::get_session_item(&state.0, key)
                .await
                .expect("failed to read a session item");
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
    let out = browse.run(
        &support::script(r#"
        const initial = app.snapshot();
        const loading = initial.find(n => n.role === "text" && n.label === "Loading") !== undefined;
        const dialog = until("the venue catalogue", (s) =>
            s.find({ role: "card", label: "Alpha Hall" }) !== undefined
                && s.find({ role: "card", label: "Beta Room" }) !== undefined
                && s.find({ role: "input", label: "Search venues…" })?.focused === true ? s : undefined);

        // Name the complete venue-dialog ring. Every focused leaf must be one
        // of these controls and geometrically inside the dialog card; the
        // title bar behind the modal is deliberately not a member.
        const focusOrder = [
            "input:Search venues…",
            "button:Join a venue",
            "button:Create venue",
            // The palette's header carries its own dismissal cap, so the ring
            // passes through it on the way to the list.
            "button:Close",
            // A venue this account owns carries its Share chip inside the row.
            "card:Alpha Hall",
            "button:Share Alpha Hall",
            "card:Beta Room",
            "button:Share Beta Room",
        ];
        const focusable = new Set(focusOrder);
        const dialogBounds = dialog.find({ role: "card", label: "Venue dialog" }).bounds;
        const contained = (outer, inner) =>
            inner.x >= outer.x - 0.5 && inner.y >= outer.y - 0.5
                && inner.x + inner.width <= outer.x + outer.width + 0.5
                && inner.y + inner.height <= outer.y + outer.height + 0.5;
        const focusedLeaf = (frame) => {
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
        // Coordinate activation of the title bar's venue trigger behind the
        // modal lands on the scrim: the covered menu must not open.
        const occludedTrigger = app.snapshot().find({ role: "button", label: "Venues" });
        app.click(occludedTrigger);
        app.frames(2);
        const afterOccludedClick = app.snapshot();

        // With no venue open the picker stays up.
        const ready = until("the venue catalogue again", (s) =>
            s.find({ role: "card", label: "Beta Room" }) !== undefined
                && s.find({ role: "input", label: "Search venues…" }) !== undefined ? s : undefined);
        const search = ready.find({ role: "input", label: "Search venues…" });
        app.type(search, "beta");
        const filtered = until("the filtered venue", (s) =>
            s.find({ role: "card", label: "Beta Room" }) !== undefined
                && s.find({ role: "card", label: "Alpha Hall" }) === undefined);
        app.click(filtered.find({ role: "card", label: "Beta Room" }));
        until("Beta's song list", (s) =>
            s.find({ role: "button", label: "Beta Room" }) !== undefined
                && s.find({ role: "row", label: "Beta initial" }) !== undefined);

        // The title bar's venue menu: open it, and pick a row once its
        // catalogue has landed.
        const menuRow = (name) => {
            nav.venueMenu();
            const menu = until(`${name} in the venue menu`, (s) =>
                s.find({ role: "row", label: name }) !== undefined ? s : undefined);
            app.click(menu.find({ role: "row", label: name }));
        };

        // Start Alpha, then leave it before its delayed result lands.
        menuRow("Alpha Hall");
        until("Alpha's loading song list", (s) =>
            s.find({ role: "button", label: "Alpha Hall" }) !== undefined);
        menuRow("Beta Room");
        until("Beta after the replacement", (s) =>
            s.find({ role: "row", label: "Beta current" }) !== undefined);
        app.frames(20, { waitMs: 60 });

        // A dismissed slow catalogue request cannot overwrite the next menu.
        nav.venueMenu();
        until("the stale catalogue loading menu", (s) =>
            s.find({ role: "text", label: "Loading venues…" }) !== undefined);
        app.action("luma::DismissOverlay");
        until("the menu gone", (s) => s.find({ role: "row", label: "New venue…" }) === undefined);
        nav.venueMenu();
        until("the current venue menu", (s) => s.find({ role: "row", label: "Beta Room" }) !== undefined);
        app.frames(10, { waitMs: 50 });
        const afterStaleCatalogue = app.snapshot();
        app.action("luma::DismissOverlay");

        // Two reads of the same venue: the second answer must win even though
        // the first lands later. The menu does nothing for the venue that is
        // already open, so each read is a return from another venue.
        menuRow("Alpha Hall");
        until("Alpha", (s) => s.find({ role: "button", label: "Alpha Hall" }) !== undefined);
        menuRow("Beta Room");
        until("Beta, loading", (s) => s.find({ role: "button", label: "Beta Room" }) !== undefined);
        menuRow("Alpha Hall");
        until("Alpha again", (s) => s.find({ role: "button", label: "Alpha Hall" }) !== undefined);
        menuRow("Beta Room");
        until("the same-id latest row", (s) =>
            s.find({ role: "row", label: "Beta same-id latest" }) !== undefined);
        app.frames(10, { waitMs: 50 });
        const settled = app.snapshot();
        ({ loading,
           searched: filtered.findAll({ role: "card" })
               .map((n) => n.label).filter((label) => label === "Beta Room"),
           staleSafe: settled.find({ role: "button", label: "Beta Room" }) !== undefined
               && settled.find({ role: "row", label: "Beta same-id latest" }) !== undefined
               && settled.find({ role: "row", label: "Beta same-id stale" }) === undefined
               && settled.find({ role: "row", label: "Alpha stale" }) === undefined,
           staleCatalogueRejected:
               afterStaleCatalogue.find((n) => n.role === "text" && n.label.includes("stale catalogue failure")) === undefined,
           focusContract: focusTrace[0] === focusOrder[0]
               && reverseLast === focusOrder[focusOrder.length - 1]
               && reverseFirst === focusOrder[0]
               && forwardFirst === focusOrder[0],
           focusTrace,
           occluded: afterOccludedClick.find({ role: "row", label: "New venue…" }) === undefined })
    "#),
        Duration::from_secs(90),
    );
    assert_eq!(out["loading"], true);
    assert_eq!(out["searched"], serde_json::json!(["Beta Room"]));
    assert_eq!(out["staleSafe"], true);
    assert_eq!(
        out["focusTrace"],
        serde_json::json!([
            "input:Search venues…",
            // shift-tab from the first stop wraps to the last…
            "button:Share Beta Room",
            "input:Search venues…",
            "button:Join a venue",
            "button:Create venue",
            "button:Close",
            "card:Alpha Hall",
            "button:Share Alpha Hall",
            "card:Beta Room",
            "button:Share Beta Room",
            "input:Search venues…"
        ])
    );
    for key in ["staleCatalogueRejected", "focusContract", "occluded"] {
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
    let out = restored.run(
        &support::script(r#"
        const loading = app.snapshot().find(n => n.role === "text" && n.label === "Loading") !== undefined;
        let pickerSeen = false;
        const shot = until("the restored Beta venue", (s) => {
            pickerSeen ||= s.find({ role: "card", label: "Venue dialog" }) !== undefined;
            return s.find({ role: "button", label: "Beta Room" }) !== undefined
                && s.find({ role: "row", label: "Beta Room Track" }) !== undefined;
        });
        ({ loading, pickerSeen,
           restored: shot.find({ role: "card", label: "Beta Room" }) === undefined,
           track: shot.find({ role: "row", label: "Beta Room Track" }) !== undefined })
    "#),
        Duration::from_secs(90),
    );
    assert_eq!(out["loading"], true);
    assert_eq!(
        out["pickerSeen"], false,
        "restoration must never paint the picker"
    );
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
    let out = stale.run(
        &support::script(r#"
        const loading = app.snapshot().find(n => n.role === "text" && n.label === "Loading") !== undefined;
        const shot = until("the stale preference fallback", (s) =>
            s.find({ role: "card", label: "Present Venue" }) !== undefined);
        app.frames(4, { waitMs: 50 });
        ({ loading,
           picker: shot.find({ role: "input", label: "Search venues…" }) !== undefined,
           onboarding: shot.find({ role: "text", label: "Create your first venue" }) === undefined })
    "#),
        Duration::from_secs(90),
    );
    assert_eq!(out["loading"], true);
    assert_eq!(out["picker"], true);
    assert_eq!(out["onboarding"], true);
    drop(stale);
    assert_eq!(session_item(&stale_dir, LAST_VENUE), None);

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
    let out = empty.run(
        &support::script(r#"
        const loading = app.snapshot().find(n => n.role === "text" && n.label === "Loading") !== undefined;
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
        // A new venue has no rig, so creating it closes the dialog onto its
        // setup page: Rig.
        const setup = until("the new venue's setup page", (s) =>
            s.find({ role: "card", label: "Venue dialog" }) === undefined
                && s.find({ role: "card", label: "Morning Room Venue" }) !== undefined);
        // Arrange is its (empty) song list.
        nav.mode("Arrange");
        const opened = until("the new venue's empty song list", (s) =>
            s.find({ role: "text", label: "No tracks imported" }) !== undefined ? s : undefined);
        ({ loading,
           focused: create.find({ role: "input", label: "Venue name" }).focused,
           opened: opened.find({ role: "text", label: "No tracks imported" }) !== undefined,
           setup: !!setup })
    "#),
        Duration::from_secs(90),
    );
    assert_eq!(out["loading"], true);
    assert_eq!(out["focused"], true);
    assert_eq!(out["opened"], true);
    assert_eq!(out["setup"], true);
    drop(empty);
    let mut created_restore = harness(&empty_dir, luma_app::NavigationFixture::default());
    let out = created_restore.run(
        &support::script(
            r#"
        const shot = until("the created venue restore", (s) =>
            s.find({ role: "button", label: "Morning Room" }) !== undefined);
        shot.find({ role: "card", label: "Morning Room" }) === undefined
    "#,
        ),
        Duration::from_secs(90),
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
    let out = fifo.run(
        &support::script(
            r#"
        let picker = until("the FIFO venue picker", (s) =>
            s.find({ role: "card", label: "Alpha Hall" }) !== undefined);
        app.click(picker.find({ role: "card", label: "Alpha Hall" }));
        until("the FIFO Alpha venue", (s) =>
            s.find({ role: "button", label: "Alpha Hall" }) !== undefined);
        nav.switchVenue("Beta Room");
        const beta = until("the FIFO Beta opener", (s) =>
            s.find({ role: "button", label: "Beta Room" }) !== undefined);
        app.frames(12, { waitMs: 50 });
        beta.find({ role: "button", label: "Beta Room" }) !== undefined
        "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(out, Value::Bool(true));
    drop(fifo);
    assert_eq!(session_item(&fifo_dir, LAST_VENUE).as_deref(), Some("beta"));
    let mut fifo_restore = harness(
        &fifo_dir,
        luma_app::NavigationFixture {
            venues_delay: Duration::from_millis(180),
            ..Default::default()
        },
    );
    let out = fifo_restore.run(
        &support::script(r#"
        const loading = app.snapshot().find(n => n.role === "text" && n.label === "Loading") !== undefined;
        const shot = until("the FIFO final selection restore", (s) =>
            s.find({ role: "button", label: "Beta Room" }) !== undefined);
        ({ loading,
           beta: shot.find({ role: "button", label: "Beta Room" }) !== undefined,
           picker: shot.find({ role: "card", label: "Beta Room" }) !== undefined })
        "#),
        Duration::from_secs(90),
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
    let out = failed.run(
        &support::script(r#"
        const loading = app.snapshot().find(n => n.role === "text" && n.label === "Loading") !== undefined;
        const shot = until("the venue error", (s) =>
            s.find({ role: "text", label: "Could not load your venues" }) !== undefined);
        ({ loading,
           // The plain line, and none of the backend's text: that is the log's.
           error: !shot.nodes.some((n) => (n.label || "").includes("fixture catalogue failure")),
           retry: shot.find({ role: "button", label: "Retry" }) !== undefined,
           empty: shot.find({ role: "text", label: "Create your first venue" }) !== undefined })
    "#),
        Duration::from_secs(90),
    );
    assert_eq!(out["loading"], true);
    assert_eq!(out["error"], true);
    assert_eq!(out["retry"], true);
    assert_eq!(out["empty"], false);
}

/// Remove one score row from a fixture library between launches.
fn delete_score(dir: &Path, id: &str) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start fixture runtime")
        .block_on(async {
            let db = luma_lib::database::local::database::init_app_db_at(dir)
                .await
                .expect("failed to reopen fixture app database");
            sqlx::query("DELETE FROM scores WHERE id = ?")
                .bind(id)
                .execute(&db.0)
                .await
                .expect("failed to delete fixture score");
            db.0.close().await;
        });
}

/// `docs/specs/comet-shell.md`: a launch reopens the last venue in the mode it
/// was left in, with the score it had open — unless that score is gone, when
/// Arrange is the song list again. (A venue switch carries the page instead,
/// so only the last venue's saved state is a launch's to restore.)
#[test]
fn the_last_venue_comes_back_in_its_mode_with_its_score_across_a_relaunch() {
    let dir = fixture_dir(
        "modes",
        &[("alpha", "Alpha Hall"), ("beta", "Beta Room")],
        None,
        true,
    );
    let mut first = harness(&dir, luma_app::NavigationFixture::default());
    first.run(
        &support::script(
            r#"
        const titled = (s, label) => s.find({ role: "button", label }) !== undefined;
        nav.venue("Alpha Hall");
        nav.track("Alpha Hall Track");
        until("Alpha's score", (s) => titled(s, "Alpha Hall Track · #1"));
        nav.switchVenue("Beta Room");
        nav.track("Beta Room Track");
        until("Beta's score", (s) => titled(s, "Beta Room Track · #1"));
        nav.mode("Rig");
        // Long enough for the debounced save to land.
        app.frames(30, { waitMs: 50 });
        true
        "#,
        ),
        Duration::from_secs(90),
    );
    drop(first);

    // Beta was the last venue, left in Rig with its score open in Arrange.
    let mut second = harness(&dir, luma_app::NavigationFixture::default());
    let out = second.run(
        &support::script(
            r#"
        const titled = (s, label) => s.find({ role: "button", label }) !== undefined;
        const beta = until("Beta, restored in Rig", (s) =>
            s.find({ role: "button", label: "Beta Room" })?.bounds.y < 44
              && s.find({ role: "card", label: "Beta Room Venue" }) !== undefined ? s : undefined);
        const rig = nav.shownMode(beta) === "Rig";
        nav.mode("Arrange");
        const score = until("Beta's score, restored", (s) =>
            titled(s, "Beta Room Track · #1") && s.find({ role: "card", label: "Song list" }) === undefined);
        // Long enough for the debounced save to land.
        app.frames(30, { waitMs: 50 });
        ({ rig, score: !!score })
        "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(out["rig"], true);
    assert_eq!(out["score"], true);
    drop(second);
    delete_score(&dir, "score-beta");

    // Its score gone, Arrange is the song list again.
    let mut third = harness(&dir, luma_app::NavigationFixture::default());
    let out = third.run(
        &support::script(
            r#"
        const titled = (s, label) => s.find({ role: "button", label }) !== undefined;
        until("Beta, restored", (s) => s.find({ role: "button", label: "Beta Room" })?.bounds.y < 44
            && s.find({ role: "card", label: "Song list" }) !== undefined);
        // Long enough for a restore that wrongly kept the score to open it.
        app.frames(20, { waitMs: 50 });
        ({ betaScore: titled(app.snapshot(), "Beta Room Track · #1") })
        "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(out["betaScore"], false);
}

/// Live's deck panel keeps the width it was dragged to across a relaunch.
#[test]
fn the_deck_panel_width_survives_a_relaunch() {
    let dir = fixture_dir("decks", &[("alpha", "Alpha Hall")], None, true);
    let mut first = harness(&dir, luma_app::NavigationFixture::default());
    let dragged = first.run(
        &support::script(
            r#"
        nav.venue("Alpha Hall");
        nav.live();
        const panel = (s) => s.find({ role: "card", label: "Deck panel" })?.bounds;
        const shot = until("the deck panel", (s) =>
            panel(s) && s.find({ role: "slider", label: "Deck panel width" }) ? s : undefined);
        const before = panel(shot).width;
        const grip = shot.find({ role: "slider", label: "Deck panel width" }).bounds;
        app.drag({ x: grip.x + grip.width / 2, y: grip.y + grip.height / 2 }, { dx: 90, dy: 0 }, { steps: 8 });
        const after = until("the wider panel", (s) => panel(s).width > before + 40 ? s : undefined);
        // Long enough for the debounced save to land.
        app.frames(30, { waitMs: 50 });
        panel(after).width
        "#,
        ),
        Duration::from_secs(90),
    );
    drop(first);

    let mut second = harness(&dir, luma_app::NavigationFixture::default());
    let restored = second.run(
        &support::script(
            r#"
        until("Alpha, restored", (s) => s.find({ role: "button", label: "Alpha Hall" })?.bounds.y < 44);
        nav.live();
        const panel = (s) => s.find({ role: "card", label: "Deck panel" })?.bounds;
        panel(until("the deck panel", (s) => panel(s) ? s : undefined)).width
        "#,
        ),
        Duration::from_secs(90),
    );
    let (dragged, restored) = (dragged.as_f64().unwrap(), restored.as_f64().unwrap());
    assert!(
        (dragged - restored).abs() < 1.0,
        "dragged to {dragged}, relaunched at {restored}"
    );
}

/// A rebound shortcut is this machine's, and still bound after a relaunch:
/// the new key works once the stored rebinds land, and the old one does not.
#[test]
fn a_rebound_shortcut_survives_a_relaunch() {
    let dir = fixture_dir("shortcuts", &[("alpha", "Alpha Hall")], None, true);
    let mut first = harness(&dir, luma_app::NavigationFixture::default());
    first.run(
        &support::script(
            r#"
        nav.venue("Alpha Hall");
        app.action("luma::OpenSettings");
        nav.step("the Shortcuts section", "toggle", "Shortcuts");
        nav.step("the agent panel's recorder", "button", "View: Show or hide the agent panel");
        until("the recorder listening", (s) => s.find({ role: "text", label: "Press a key…" }));
        app.key("secondary-shift-k");
        until("the rebind", (s) => s.find({ role: "button", label: "Reset View: Show or hide the agent panel" }));
        // Long enough for the debounced save to land.
        app.frames(30, { waitMs: 50 });
        true
        "#,
        ),
        Duration::from_secs(90),
    );
    drop(first);
    let stored = session_item(&dir, "shortcuts").expect("no shortcuts were stored");
    assert!(stored.contains("view.agent"), "stored {stored}");

    let mut second = harness(&dir, luma_app::NavigationFixture::default());
    let out = second.run(
        &support::script(
            r#"
        // The agent panel's toggle: `true` up, `false` away.
        const up = (s) => s.find({ role: "button", label: "Hide agent" }) !== undefined ? true
          : s.find({ role: "button", label: "Show agent" }) !== undefined ? false : undefined;
        until("Alpha, restored", (s) => s.find({ role: "button", label: "Alpha Hall" })?.bounds.y < 44);
        // The stored rebinds have landed once Settings shows the row rebound.
        app.action("luma::OpenSettings");
        nav.step("the Shortcuts section", "toggle", "Shortcuts");
        until("the stored rebind", (s) => s.find({ role: "button", label: "Reset View: Show or hide the agent panel" }));
        nav.dismiss();
        // Rig: a page with the agent panel (the song list has none to toggle).
        app.action("luma::ShowRig");
        const start = up(until("the shell", (s) => up(s) !== undefined
          && s.find({ role: "card", label: "Alpha Hall Venue" }) ? s : undefined));
        app.key("secondary-shift-k");
        until("the new key, after a relaunch", (s) => up(s) === !start);
        app.key("secondary-shift-k");
        until("the panel back", (s) => up(s) === start);
        app.key("secondary-b");
        app.frames(6);
        ({ oldKeyIgnored: up(app.snapshot()) === start })
        "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(out["oldKeyIgnored"], true);
}
