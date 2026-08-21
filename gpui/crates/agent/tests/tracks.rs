//! The track browser, driven end to end against a seeded library.
//!
//! The point of this test is the *filters*: `list_tracks_enriched` returns the
//! whole visible library and decorates it with one venue's clip counts, so
//! every number on this screen is the view's arithmetic over that. A fixture
//! with known counts is the only way to tell a working filter from a missing
//! one — which is the bug this test was written for, where opening a venue
//! listed the entire library.
//!
//! # Why the fixture is written in SQL
//!
//! The seam has no way to create a track without an audio file to import and
//! analyse, and none to place a clip without a pattern graph to place. Both are
//! minutes of work per row for facts this screen only counts. So the fixture
//! writes rows directly, exactly as the Rust services' own tests do — and it
//! writes them *before* admission is armed, which is the window the app
//! database leaves open for a host to populate itself.

#![cfg(feature = "app")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode};
use serde_json::Value;
use sqlx::SqlitePool;

/// Tracks that are somebody else's. Only visible at all because they carry a
/// clip in a venue this host can read.
const OTHER_USER: &str = "another-principal";
/// Enough rows that a screenful is a small fraction of them: the virtualized
/// list must build a window, not a library.
const FILLER: usize = 300;

// -- the fixture --------------------------------------------------------------

/// One track's identity in the fixture, and where its clips are.
struct Seed {
    id: &'static str,
    title: &'static str,
    artist: &'static str,
    uid: Option<&'static str>,
    /// The venue whose score carries this track's one clip, if any.
    clips_in: Option<&'static str>,
}

/// Newest first, which is the order `list_tracks_enriched` returns and the
/// order the browser shows.
const SEEDS: [Seed; 5] = [
    Seed {
        id: "track-aurora",
        title: "Aurora",
        artist: "Nightliner",
        uid: None,
        clips_in: Some("venue-main"),
    },
    Seed {
        id: "track-basslines",
        title: "Basslines",
        artist: "Nightliner",
        uid: None,
        clips_in: Some("venue-main"),
    },
    Seed {
        id: "track-cascade",
        title: "Cascade",
        artist: "Sundial",
        uid: None,
        clips_in: None,
    },
    Seed {
        id: "track-drift",
        title: "Drift",
        artist: "Sundial",
        uid: None,
        clips_in: Some("venue-other"),
    },
    Seed {
        id: "track-echoes",
        title: "Echoes",
        artist: "Guest",
        uid: Some(OTHER_USER),
        clips_in: Some("venue-main"),
    },
];

/// What each filter combination should admit, derived from [`SEEDS`] and
/// [`FILLER`] rather than restated as literals — a fixture that disagreed with
/// its own expectations would prove nothing.
struct Expected;

impl Expected {
    /// Mine and in this venue: the browser's default.
    const DEFAULT: usize = 2 + FILLER;
    /// Everyone's, in this venue.
    const IN_VENUE: usize = 3 + FILLER;
    /// Mine, anywhere.
    const MINE: usize = 4 + FILLER;
    /// The whole visible library.
    const ALL: usize = 5 + FILLER;
}

async fn seed(config_dir: &Path) {
    let db = luma_lib::database::local::database::init_app_db_at(config_dir)
        .await
        .expect("failed to open the fixture database");
    let pool = &db.0;

    for (id, name) in [("venue-main", "Test Venue"), ("venue-other", "Other Venue")] {
        run(
            pool,
            "INSERT INTO venues (id, uid, name) VALUES (?, NULL, ?)",
            [id, name],
        )
        .await;
    }
    run(
        pool,
        "INSERT INTO patterns (id, uid, name) VALUES ('pattern', NULL, 'Strobe')",
        [],
    )
    .await;

    // `created_at` is the browser's sort key, so the fixture sets it rather
    // than letting every row share one CURRENT_TIMESTAMP: the named tracks are
    // newest, in the order `SEEDS` declares them, and the filler follows.
    for (index, track) in SEEDS.iter().enumerate() {
        let created_at = format!("2026-08-19T12:{:02}:00Z", 59 - index);
        insert_track(
            pool,
            track.id,
            track.title,
            track.artist,
            track.uid,
            &created_at,
        )
        .await;
        if let Some(venue) = track.clips_in {
            insert_clip(pool, track.id, venue).await;
        }
    }
    for index in 0..FILLER {
        let id = format!("track-filler-{index:03}");
        let title = format!("Filler {index:03}");
        let created_at = format!("2020-01-01T00:{:02}:00Z", index % 60);
        insert_track(pool, &id, &title, "Filler", None, &created_at).await;
        insert_clip(pool, &id, "venue-main").await;
    }
    pool.close().await;
}

async fn insert_track(
    pool: &SqlitePool,
    id: &str,
    title: &str,
    artist: &str,
    uid: Option<&str>,
    created_at: &str,
) {
    sqlx::query(
        "INSERT INTO tracks (id, uid, track_hash, title, artist, duration_seconds, file_path, created_at)
         VALUES (?, ?, ?, ?, ?, 240.0, ?, ?)",
    )
    .bind(id)
    .bind(uid)
    .bind(format!("{id}-hash"))
    .bind(title)
    .bind(artist)
    .bind(format!("/fixture/{id}.mp3"))
    .bind(created_at)
    .execute(pool)
    .await
    .expect("failed to seed a track");
}

/// One score with one clip on it: the clip is what `venue_annotation_count`
/// counts, and therefore what "in venue" means.
async fn insert_clip(pool: &SqlitePool, track: &str, venue: &str) {
    let score = format!("score-{track}-{venue}");
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name) VALUES (?, NULL, ?, ?, 'Score')",
    )
    .bind(&score)
    .bind(track)
    .bind(venue)
    .execute(pool)
    .await
    .expect("failed to seed a score");
    sqlx::query(
        "INSERT INTO track_scores (id, uid, score_id, pattern_id, start_time, end_time)
         VALUES (?, NULL, ?, 'pattern', 0.0, 60.0)",
    )
    .bind(format!("clip-{score}"))
    .bind(&score)
    .execute(pool)
    .await
    .expect("failed to seed a clip");
}

async fn run<const N: usize>(pool: &SqlitePool, sql: &'static str, binds: [&str; N]) {
    let mut query = sqlx::query(sql);
    for bind in binds {
        query = query.bind(bind);
    }
    query.execute(pool).await.expect("fixture insert failed");
}

// -- the harness --------------------------------------------------------------

/// A library of its own, seeded, so the run cannot see — or corrupt — the
/// developer's. Named after the process so two runs never share one.
fn fixture_config_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-tracks-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("failed to create the temporary config directory");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start the fixture runtime")
        .block_on(seed(&dir));
    dir
}

fn harness() -> Harness {
    std::env::set_var("LUMA_CONFIG_DIR", fixture_config_dir());
    let root: gpui_agent::RootFactory = Arc::new(|_: &mut Window, cx: &mut App| -> AnyView {
        luma_app::init(cx);
        let library = luma_app::Library::open().expect("failed to open the fixture library");
        cx.new(|cx| luma_app::Luma::new(library, cx)).into()
    });
    Harness::headless(
        Config {
            mode: Mode::Headless,
            call_timeout: Duration::from_secs(20),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start the harness")
}

/// Open the venue, read the default view, work the two filters, then search.
/// Every reading is `{count, rows}`: the count is what the toolbar claims and
/// the rows are what the virtualized list actually built, so a filter that
/// only changed the label would not pass.
const SCRIPT: &str = r#"
    function read() {
        const shot = app.snapshot();
        return {
            count: shot.find((node) => node.label.endsWith("TRACKS")).label,
            rows: shot.findAll({ role: "row" }).map((node) => node.label),
        };
    }

    function press(role, label) {
        app.click(app.snapshot().find({ role, label }));
        app.frames(2);
        return read();
    }

    const home = app.snapshot();
    const entry = {
        wordmark: home.find({ role: "text", label: "luma" }) !== undefined,
        venues: home.findAll({ role: "card" }).map((node) => node.label),
        rows: home.findAll({ role: "row" }).length,
    };

    app.click(home.find({ role: "card", label: "Test Venue" }));
    app.frames(6);
    const opened = read();

    const all = press("toggle", "All");
    const anywhere = press("toggle", "In Venue");
    const mine = press("toggle", "Mine");

    app.type(app.snapshot().find({ role: "input" }), "drif");
    app.frames(2);
    const searched = read();

    // Escape clears the field, which puts the screen back where it was.
    app.key("escape");
    app.frames(2);
    const cleared = read();

    ({ entry, opened, all, anywhere, mine, searched, cleared })
"#;

#[test]
fn the_browser_filters_a_seeded_library_by_venue_ownership_and_search() {
    let mut harness = harness();
    let result = harness.exec(SCRIPT, Duration::from_secs(120));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;

    // 1. The app enters on the venue grid, not on a track table.
    assert_eq!(out["entry"]["wordmark"], true);
    assert_eq!(out["entry"]["rows"], 0);
    assert_eq!(
        out["entry"]["venues"],
        serde_json::json!(["Test Venue", "Other Venue"])
    );

    // 2. Opening a venue lists that venue's tracks, not the whole library.
    assert_eq!(
        out["opened"]["count"],
        format!("{} TRACKS", Expected::DEFAULT)
    );
    assert_eq!(rows(&out["opened"])[..2], ["Aurora", "Basslines"]);

    // 3. Each filter axis moves the count by exactly the rows it admits.
    assert_eq!(
        out["all"]["count"],
        format!("{} TRACKS", Expected::IN_VENUE)
    );
    assert_eq!(
        out["anywhere"]["count"],
        format!("{} TRACKS", Expected::ALL)
    );
    assert_eq!(out["mine"]["count"], format!("{} TRACKS", Expected::MINE));

    // 4. Search narrows to what it matches, and Escape puts it back.
    assert_eq!(out["searched"]["count"], "1 TRACKS");
    assert_eq!(rows(&out["searched"]), ["Drift"]);
    assert_eq!(
        out["cleared"]["count"],
        format!("{} TRACKS", Expected::MINE)
    );

    // 5. The list is virtualized: a screenful of rows, not a library of them.
    let built = rows(&out["opened"]).len();
    assert!(built > 0, "the list built no rows at all");
    assert!(
        built < Expected::DEFAULT / 4,
        "the list built {built} of {} rows — it is not virtualizing",
        Expected::DEFAULT
    );
}

fn rows(reading: &Value) -> Vec<String> {
    serde_json::from_value(reading["rows"].clone()).expect("a reading has rows")
}
