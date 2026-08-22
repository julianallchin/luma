//! Data seams needed by venue restore and the add-track dialog.

#![cfg(feature = "app")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde_json::json;

fn environment_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

fn disposable_config_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "luma-gpui-library-foundation-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn library_creates_idempotent_empty_membership_and_persists_session_items() {
    let _environment = environment_lock();
    let config = disposable_config_dir();
    std::env::set_var("LUMA_CONFIG_DIR", &config);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let db = luma_lib::database::local::database::init_app_db_at(&config)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, title, file_path)
             VALUES ('track', NULL, 'hash', 'Empty score', '/track.mp3')",
        )
        .execute(&db.0)
        .await
        .unwrap();
        db.0.close().await;
    });

    let library = luma_app::Library::open().unwrap();
    runtime.block_on(async {
        let venue = library.create_venue("Venue", None).await.unwrap();
        let first = library
            .create_score(
                "f4034256-5f2f-4c6a-b5df-f64aac717ce8",
                "track",
                &venue.id,
                None,
            )
            .await
            .unwrap();
        let replay = library
            .create_score(
                "f4034256-5f2f-4c6a-b5df-f64aac717ce8",
                "track",
                &venue.id,
                None,
            )
            .await
            .unwrap();
        assert_eq!(first.id, replay.id, "request replay created a second score");

        let first_add = library
            .ensure_track_in_venue(
                "484d62bc-6088-47bb-989d-61e91f57b727",
                "track",
                &venue.id,
                None,
            )
            .await
            .unwrap();
        let repeated_add = library
            .ensure_track_in_venue(
                "2cd7fd0a-a1ea-4a12-aee2-0c5995428763",
                "track",
                &venue.id,
                None,
            )
            .await
            .unwrap();
        assert_eq!(first.id, first_add.id);
        assert_eq!(first_add.id, repeated_add.id);

        let rows = library.tracks(&venue.id).await.unwrap();
        let row = rows.iter().find(|row| row.id == "track").unwrap();
        assert!(row.is_in_venue);
        assert_eq!(row.venue_score_count, 1);
        assert_eq!(row.venue_annotation_count, 0);

        let another_score = library
            .create_score(
                "83d83dd9-09ab-4fdb-9d19-aa15c612c347",
                "track",
                &venue.id,
                Some("Alternate"),
            )
            .await
            .unwrap();
        assert_ne!(first.id, another_score.id);
        let rows = library.tracks(&venue.id).await.unwrap();
        assert_eq!(rows[0].venue_score_count, 2);

        assert_eq!(library.get_session_item("last-venue").await.unwrap(), None);
        library
            .set_session_item("last-venue", &venue.id)
            .await
            .unwrap();
        assert_eq!(
            library.get_session_item("last-venue").await.unwrap(),
            Some(venue.id)
        );
        library.remove_session_item("last-venue").await.unwrap();
        assert_eq!(library.get_session_item("last-venue").await.unwrap(), None);
    });
}

#[test]
fn both_dj_adapters_normalize_every_browser_read_through_library() {
    let _environment = environment_lock();
    let config = disposable_config_dir();
    std::env::set_var("LUMA_CONFIG_DIR", &config);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let mut library = luma_app::Library::open().unwrap();
    library.set_source_adapter_fixture(luma_app::SourceAdapterFixture {
        library: json!({"databaseUuid": "engine-db", "libraryPath": "/engine", "trackCount": 1}),
        playlists: json!([{"id": 7, "title": "Engine crate", "parentId": null, "trackCount": 1}]),
        tracks: json!([engine_track()]),
        playlist_tracks: HashMap::from([("7".into(), json!([engine_track()]))]),
        searches: HashMap::from([("needle".into(), json!([engine_track()]))]),
    });
    runtime.block_on(assert_source_contract(
        &library,
        luma_app::TrackSource::EngineDj {
            library_path: "/ignored-by-fixture".into(),
        },
        Some("engine-db"),
        "7",
        "Engine crate",
        "42",
    ));

    library.set_source_adapter_fixture(luma_app::SourceAdapterFixture {
        library: json!({"trackCount": 1}),
        playlists: json!([{"id": "crate", "name": "Rekordbox crate", "parentId": null, "trackCount": 1}]),
        tracks: json!([rekordbox_track()]),
        playlist_tracks: HashMap::from([("crate".into(), json!([rekordbox_track()]))]),
        searches: HashMap::from([("needle".into(), json!([rekordbox_track()]))]),
    });
    runtime.block_on(assert_source_contract(
        &library,
        luma_app::TrackSource::Rekordbox,
        None,
        "crate",
        "Rekordbox crate",
        "rb-uuid",
    ));
}

fn engine_track() -> serde_json::Value {
    json!({
        "id": 42,
        "path": "/music/needle.wav",
        "filename": "needle.wav",
        "title": "Needle",
        "artist": "Artist",
        "album": "Album",
        "bpmAnalyzed": 128.0,
        "length": 180.0,
        "originDatabaseUuid": null,
        "originTrackId": null
    })
}

fn rekordbox_track() -> serde_json::Value {
    json!({
        "id": "content-id",
        "uuid": "rb-uuid",
        "filePath": "/music/needle.wav",
        "filename": "needle.wav",
        "title": "Needle",
        "artist": "Artist",
        "album": "Album",
        "bpm": 128.0,
        "durationSeconds": 180.0,
        "fileSize": 12,
        "sampleRate": 48000
    })
}

async fn assert_source_contract(
    library: &luma_app::Library,
    source: luma_app::TrackSource,
    identity: Option<&str>,
    playlist_id: &str,
    playlist_name: &str,
    track_id: &str,
) {
    let opened = library.source_library(source.clone()).await.unwrap();
    assert_eq!(opened.identity.as_deref(), identity);
    assert_eq!(opened.track_count, 1);

    let playlists = library.source_playlists(source.clone()).await.unwrap();
    assert_eq!(playlists.len(), 1);
    assert_eq!(playlists[0].id, playlist_id);
    assert_eq!(playlists[0].name, playlist_name);
    assert_eq!(playlists[0].track_count, 1);

    for tracks in [
        library.source_tracks(source.clone()).await.unwrap(),
        library
            .source_playlist_tracks(source.clone(), playlist_id)
            .await
            .unwrap(),
        library
            .search_source_tracks(source, "needle")
            .await
            .unwrap(),
    ] {
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, track_id);
        assert_eq!(tracks[0].title.as_deref(), Some("Needle"));
        assert_eq!(tracks[0].artist.as_deref(), Some("Artist"));
        assert_eq!(tracks[0].bpm, Some(128.0));
        assert_eq!(tracks[0].duration_seconds, Some(180.0));
    }
}
