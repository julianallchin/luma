//! Data seams needed by venue restore and the add-track dialog.

#![cfg(feature = "app")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::json;

fn environment_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

fn write_silent_wav(path: &std::path::Path, frames: u32) {
    let data_len = frames * 2;
    let mut bytes = Vec::with_capacity((44 + data_len) as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8_000_u32.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize((44 + data_len) as usize, 0);
    std::fs::write(path, bytes).unwrap();
}

#[cfg(unix)]
fn install_slow_failing_python(cache: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let bin = cache.join("python-env/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let python = bin.join("python3");
    std::fs::write(
        &python,
        "#!/bin/sh\nsleep 0.5\ncase \"$1\" in\n  *beat_worker.py) printf '%s\\n' '{\"beats\":[0.0,0.5,1.0],\"downbeats\":[0.0,1.0],\"bpm\":120.0,\"downbeat_offset\":0.0,\"beats_per_bar\":4}' ;;\n  *) exit 1 ;;\nesac\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&python).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(python, permissions).unwrap();
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

#[cfg(unix)]
#[test]
fn both_dj_sources_import_through_the_same_library_request_contract() {
    let _environment = environment_lock();
    let config = disposable_config_dir();
    let cache = config.join("cache");
    install_slow_failing_python(&cache);
    std::env::set_var("LUMA_CONFIG_DIR", &config);
    std::env::set_var("LUMA_CACHE_DIR", &cache);
    let engine_audio = config.join("engine.wav");
    let rekordbox_audio = config.join("rekordbox.wav");
    write_silent_wav(&engine_audio, 4_000);
    write_silent_wav(&rekordbox_audio, 5_000);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut library = luma_app::Library::open().unwrap();

    let mut engine = engine_track();
    engine["path"] = json!(engine_audio);
    library.set_source_adapter_fixture(luma_app::SourceAdapterFixture {
        library: json!({"databaseUuid": "engine-db", "libraryPath": "/engine", "trackCount": 1}),
        playlists: json!([]),
        tracks: json!([engine]),
        playlist_tracks: HashMap::new(),
        searches: HashMap::new(),
    });
    runtime.block_on(async {
        let result = library
            .import_tracks(luma_app::TrackImportRequest::Source {
                source: luma_app::TrackSource::EngineDj {
                    library_path: "/fixture".into(),
                },
                track_ids: vec!["42".into()],
            })
            .await
            .unwrap();
        assert_eq!(result.tracks.len(), 1);
        assert_eq!(result.tracks[0].source_type.as_deref(), Some("engine_dj"));
        assert_eq!(result.tracks[0].source_id.as_deref(), Some("engine-db:42"));
    });

    let mut rekordbox = rekordbox_track();
    rekordbox["filePath"] = json!(rekordbox_audio);
    library.set_source_adapter_fixture(luma_app::SourceAdapterFixture {
        library: json!({"trackCount": 1}),
        playlists: json!([]),
        tracks: json!([rekordbox]),
        playlist_tracks: HashMap::new(),
        searches: HashMap::new(),
    });
    runtime.block_on(async {
        let result = library
            .import_tracks(luma_app::TrackImportRequest::Source {
                source: luma_app::TrackSource::Rekordbox,
                track_ids: vec!["rb-uuid".into()],
            })
            .await
            .unwrap();
        assert_eq!(result.tracks.len(), 1);
        assert_eq!(result.tracks[0].source_type.as_deref(), Some("rekordbox"));
        assert_eq!(result.tracks[0].source_id.as_deref(), Some("rb-uuid"));
        assert_eq!(library.all_tracks().await.unwrap().len(), 2);
    });
}

#[cfg(unix)]
#[test]
fn import_returns_durable_rows_before_analysis_and_reports_typed_partial_progress() {
    let _environment = environment_lock();
    let config = disposable_config_dir();
    let cache = config.join("cache");
    install_slow_failing_python(&cache);
    std::env::set_var("LUMA_CONFIG_DIR", &config);
    std::env::set_var("LUMA_CACHE_DIR", &cache);
    let audio = config.join("tiny.wav");
    write_silent_wav(&audio, 8_000);
    let missing = config.join("missing.wav");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let library = luma_app::Library::open().unwrap();
    let mut progress = library.import_progress();
    runtime.block_on(async {
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            library.import_tracks(luma_app::TrackImportRequest::Files(vec![
                audio.clone(),
                missing.clone(),
            ])),
        )
        .await
        .expect("phase one must return promptly")
        .unwrap();
        assert_eq!(result.tracks.len(), 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].source_id, missing.to_string_lossy());

        let mut saw_analysis = false;
        let mut saw_complete = false;
        let mut saw_partial_failure = false;
        let mut terminal = None;
        while let Ok(event) = progress.try_recv() {
            if event.import_id == result.import_id {
                assert_eq!(
                    event.total, 2,
                    "analysis progress lost the original requested denominator"
                );
                saw_analysis |= event.phase == luma_app::TrackImportPhase::Analyzing;
                saw_partial_failure |= event.error.is_some();
                assert_ne!(
                    event.phase,
                    luma_app::TrackImportPhase::Complete,
                    "phase one did not return before background analysis"
                );
            }
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            while !saw_complete {
                let event = progress.recv().await.unwrap();
                if event.import_id != result.import_id {
                    continue;
                }
                assert_eq!(
                    event.total, 2,
                    "terminal progress lost the original requested denominator"
                );
                saw_analysis |= event.phase == luma_app::TrackImportPhase::Analyzing;
                if event.phase == luma_app::TrackImportPhase::Complete {
                    saw_complete = true;
                    terminal = Some(event);
                }
            }
        })
        .await
        .expect("background analysis must complete independently");
        assert!(
            saw_analysis,
            "progress never entered the typed analyzing phase"
        );
        assert!(
            saw_partial_failure,
            "partial failure was not published structurally"
        );
        let terminal = terminal.expect("complete event was retained");
        assert_eq!(terminal.done, 2, "terminal count skipped requested work");
        assert!(
            terminal.error.is_some(),
            "failed analysis was hidden by a successful terminal event"
        );

        let rows = library.all_tracks().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_type.as_deref(), Some("file"));
        assert_eq!(rows[0].bpm, Some(120.0), "completed row was not enriched");

        let duplicate = library
            .import_tracks(luma_app::TrackImportRequest::Files(vec![audio.clone()]))
            .await
            .unwrap();
        assert_eq!(duplicate.tracks[0].id, result.tracks[0].id);
        assert_eq!(library.all_tracks().await.unwrap().len(), 1);
        let duplicate_terminal = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = progress.recv().await.unwrap();
                if event.import_id == duplicate.import_id
                    && event.phase == luma_app::TrackImportPhase::Complete
                {
                    break event;
                }
            }
        })
        .await
        .expect("deduplicated import did not complete");
        assert_eq!((duplicate_terminal.done, duplicate_terminal.total), (1, 1));
        assert_eq!(duplicate_terminal.error, None);
    });
}

#[cfg(unix)]
#[test]
fn dropping_import_future_does_not_cancel_service_owned_analysis() {
    let _environment = environment_lock();
    let config = disposable_config_dir();
    let cache = config.join("cache");
    install_slow_failing_python(&cache);
    std::env::set_var("LUMA_CONFIG_DIR", &config);
    std::env::set_var("LUMA_CACHE_DIR", &cache);
    let audio = config.join("detached.wav");
    write_silent_wav(&audio, 8_000);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let library = luma_app::Library::open().unwrap();
    let mut progress = library.import_progress();
    let request = library.import_tracks(luma_app::TrackImportRequest::Files(vec![audio]));
    drop(request);
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if progress.recv().await.unwrap().phase == luma_app::TrackImportPhase::Complete {
                    break;
                }
            }
        })
        .await
        .expect("dropping the caller future cancelled the import task");
        assert_eq!(library.all_tracks().await.unwrap().len(), 1);
    });
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
