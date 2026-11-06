use crate::dispatch::{AppServices, CommandError};
use crate::models::beat_grid::BeatGrid;
use crate::models::tracks::{TrackBrowserRow, TrackImportResult, TrackSummary};
use crate::services::tracks::{self as track_service, ImportRequest};

/// The whole library, unscoped and without album-art bytes.
pub async fn list_tracks(services: &AppServices) -> Result<Vec<TrackSummary>, CommandError> {
    Ok(track_service::list_tracks(&services.db.0).await?)
}

/// `None` when beat detection has not run for this track.
pub async fn get_track_beats(
    services: &AppServices,
    track_id: String,
) -> Result<Option<BeatGrid>, CommandError> {
    Ok(track_service::get_track_beats(&services.db.0, &track_id).await?)
}

use crate::models::tracks::{BeatValidation, BeatValidationReason, BeatValidationVerdict};

/// The reviewed reference grid, retained across reanalysis.
pub async fn get_track_beat_validation(
    services: &AppServices,
    track_id: String,
) -> Result<Option<BeatValidation>, CommandError> {
    Ok(crate::database::local::beat_validations::get(&services.db.0, &track_id).await?)
}

pub async fn set_track_beat_validation(
    services: &AppServices,
    track_id: String,
    grid: BeatGrid,
    verdict: BeatValidationVerdict,
    reason: Option<BeatValidationReason>,
) -> Result<(), CommandError> {
    Ok(crate::database::local::beat_validations::set(
        &services.db.0,
        &track_id,
        &grid,
        verdict,
        reason,
    )
    .await?)
}

/// The track browser's rows: the whole library, plus the per-venue annotation
/// counts and coverage when `venue_id` is given.
///
/// `album_art_path` is a path on disk, not bytes — a bulk response stays small
/// and the client lazy-loads the image itself.
pub async fn list_tracks_enriched(
    services: &AppServices,
    venue_id: Option<String>,
) -> Result<Vec<TrackBrowserRow>, CommandError> {
    Ok(track_service::list_tracks_enriched(&services.db.0, venue_id.as_deref()).await?)
}

/// Import local files; see [`track_service::Importer::import`].
pub async fn import_tracks(
    services: &AppServices,
    file_paths: Vec<String>,
) -> Result<TrackImportResult, CommandError> {
    let principal = services.require_session().await?;
    let requests = file_paths
        .into_iter()
        .map(|path| Ok(ImportRequest::File(path)));
    Ok(importer(services)
        .import(principal, "file", requests.collect())
        .await?)
}

/// The import driver over this host's catalog, storage and analysis tasks.
pub(crate) fn importer(services: &AppServices) -> track_service::Importer<'_> {
    track_service::Importer {
        pool: &services.db.0,
        storage: &services.storage,
        workers: &services.workers,
        events: &services.events,
        tasks: &services.analysis_tasks,
    }
}

/// A DJ library row's cover, as encoded image bytes — see
/// [`crate::services::artwork`].
pub async fn source_track_artwork(
    art: Option<crate::services::artwork::ArtworkRef>,
    audio_path: Option<String>,
) -> Result<Option<Vec<u8>>, CommandError> {
    Ok(crate::services::artwork::artwork_bytes(art, audio_path).await?)
}

#[cfg(test)]
mod import_tests {
    use std::path::Path;
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::database::local::auth;
    use crate::dispatch::{Event, EventSink, Events};
    use crate::models::tracks::{TrackImportPhase, TrackImportProgress};

    struct ChannelEvents {
        tx: tokio::sync::mpsc::UnboundedSender<TrackImportProgress>,
        inserted: Arc<Barrier>,
    }

    impl EventSink for ChannelEvents {
        fn emit(&self, event: Event) {
            let Event::TrackImportState(progress) = event else {
                return;
            };
            let inserted = progress.track_id.is_some();
            let _ = self.tx.send(progress);
            if inserted {
                self.inserted.wait();
            }
        }
    }

    fn write_wav(path: &Path) {
        let frames = 8_000_u32;
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

    /// The principal the import writes as. A track is a synced row and a
    /// synced row has an owner; `import_tracks` asks `require_session` for it.
    /// Signed in for real rather than pinned with `with_fixture_principal`,
    /// because what is under test is the identity transition closing the gate,
    /// and that transition cross-checks the gate against the stored session.

    async fn services(directory: &Path, events: Events) -> crate::dispatch::SharedServices {
        crate::test_support::signed_in(directory, directory.to_path_buf())
            .await
            .with_events(events)
            .into_shared()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identity_epoch_cancellation_rolls_back_catalog_and_managed_audio() {
        let directory = tempfile::tempdir().unwrap();
        let audio = directory.path().join("source.wav");
        write_wav(&audio);
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let inserted = Arc::new(Barrier::new(2));
        let services = services(
            directory.path(),
            Events::new(ChannelEvents {
                tx: events_tx,
                inserted: Arc::clone(&inserted),
            }),
        )
        .await;
        let task_services = services.clone();
        let import = tokio::spawn(async move {
            import_tracks(&task_services, vec![audio.to_string_lossy().into_owned()]).await
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let progress = events_rx.recv().await.unwrap();
                if matches!(progress.phase, TrackImportPhase::Importing)
                    && progress.track_id.is_some()
                {
                    break;
                }
            }
        })
        .await
        .expect("import never reached the inserted-row barrier");
        // The import has already resolved its principal, matching an in-flight
        // operation when auth freezes StateDb for the identity transition.
        let mut session_guard = services.state_db.0.acquire().await.unwrap();
        let admission = auth::capture_write_admission(&services.db.0, &mut session_guard)
            .await
            .unwrap();
        let tasks = services.analysis_tasks.clone();
        let mut suspended = tasks.subscribe_identity_suspension();
        let transition_tasks = tasks.clone();
        let pool = services.db.0.clone();
        let transition = tokio::spawn(async move {
            // Production auth order: cancel/drain import compensation while
            // the old principal is still admitted, then close admission.
            let barrier = transition_tasks
                .suspend_for_identity_switch()
                .await
                .unwrap();
            auth::suspend_write_admission(&pool, &admission)
                .await
                .unwrap();
            drop(barrier);
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), suspended.changed())
            .await
            .expect("identity transition never closed analysis admission")
            .expect("identity suspension signal closed");
        assert!(tasks.current_epoch().is_err());
        inserted.wait();

        let error = import.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("identity transition"));
        transition.await.unwrap();
        let admission: (i64, i64, Option<String>) = sqlx::query_as(
            "SELECT accepting, maintenance, active_uid
             FROM auth_write_admission WHERE singleton = 1",
        )
        .fetch_one(&services.db.0)
        .await
        .unwrap();
        assert_eq!(
            admission,
            (0, 0, None),
            "production transition did not close admission"
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
            .fetch_one(&services.db.0)
            .await
            .unwrap();
        assert_eq!(count, 0, "cancelled import left catalog ownership");
        let audio_files = std::fs::read_dir(services.storage.tracks_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(audio_files, 0, "cancelled import left managed audio");
    }
}
