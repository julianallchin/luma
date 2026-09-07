//! Read-only access to the user's Rekordbox library.
//!
//! Every one of these reopens the SQLCipher `master.db` from scratch on a
//! blocking thread — `rekordbox::subprocess` keeps no cached handle — so they
//! are all `spawn_blocking` wrappers and nothing more.
//!
use std::path::Path;

use crate::dispatch::{AppServices, CommandError};
use crate::models::tracks::{TrackImportFailure, TrackImportPhase, TrackImportResult};
use crate::rekordbox::subprocess;
use crate::rekordbox::types::{RekordboxLibraryInfo, RekordboxPlaylist, RekordboxTrack};
use crate::services::tracks as track_service;

use super::tracks::{
    emit_import_progress, finish_fast_import, push_unique, rollback_after_failure,
};

/// Run a Rekordbox DB read off the async runtime.
///
/// A panic in `rbox` surfaces as a join error rather than taking the host down,
/// which is why every command below goes through here.
async fn blocking<T, F>(read: F) -> Result<T, CommandError>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(read)
        .await
        .map_err(|error| CommandError::Internal(format!("Task join error: {error}")))?
        .map_err(CommandError::Internal)
}

/// Track count of the auto-discovered library. The DB path is not caller
/// selectable, so this doubles as the existence probe.
pub async fn rekordbox_open_library(
    _services: &AppServices,
) -> Result<RekordboxLibraryInfo, CommandError> {
    blocking(subprocess::get_library_info).await
}

/// The whole library, unpaged.
pub async fn rekordbox_list_tracks(
    _services: &AppServices,
) -> Result<Vec<RekordboxTrack>, CommandError> {
    blocking(subprocess::list_tracks).await
}

/// Flat playlist list with `parent_id` back-references; the tree is assembled
/// by the caller.
pub async fn rekordbox_list_playlists(
    _services: &AppServices,
) -> Result<Vec<RekordboxPlaylist>, CommandError> {
    blocking(subprocess::list_playlists).await
}

pub async fn rekordbox_get_playlist_tracks(
    _services: &AppServices,
    playlist_id: String,
) -> Result<Vec<RekordboxTrack>, CommandError> {
    blocking(move || subprocess::get_playlist_tracks(&playlist_id)).await
}

/// Server-side search over the Rekordbox DB, uncapped.
pub async fn rekordbox_search_tracks(
    _services: &AppServices,
    query: String,
) -> Result<Vec<RekordboxTrack>, CommandError> {
    blocking(move || subprocess::search_tracks(&query)).await
}

pub async fn rekordbox_import_tracks(
    services: &AppServices,
    track_uuids: Vec<String>,
) -> Result<TrackImportResult, CommandError> {
    let import_id = uuid::Uuid::new_v4().to_string();
    let source = "rekordbox";
    let principal = services.session_user_id().await?;
    let epoch = services.analysis_tasks.current_epoch()?;
    let lease = services.analysis_tasks.lease(epoch)?;
    let guard = lease.guard();
    let source_tracks = services.track_sources.rekordbox_tracks().await?;
    let total = track_uuids.len();
    let mut tracks = Vec::new();
    let mut failures = Vec::new();
    let mut new_ids = Vec::new();

    for (done, source_id) in track_uuids.into_iter().enumerate() {
        if let Err(error) = guard.checkpoint() {
            return Err(
                rollback_after_failure(services, &new_ids, principal.as_deref(), error).await,
            );
        }
        let Some(row) = source_tracks.iter().find(|track| track.uuid == source_id) else {
            failures.push(TrackImportFailure {
                source_id: source_id.clone(),
                message: format!("Rekordbox track with UUID {source_id} not found"),
            });
            continue;
        };
        emit_import_progress(
            services,
            &import_id,
            source,
            TrackImportPhase::Importing,
            done,
            total,
            None,
            row.title.clone().or_else(|| row.filename.clone()),
            None,
        );
        let imported = match row.file_path.as_deref() {
            Some(path) if Path::new(path).exists() => {
                track_service::dj_fast_import(
                    &services.db.0,
                    &services.storage,
                    source,
                    &source_id,
                    &row.title,
                    &row.artist,
                    &row.album,
                    row.duration_seconds,
                    row.filename.as_deref(),
                    Path::new(path),
                    principal.clone(),
                )
                .await
            }
            Some(path) => Err(format!("Audio file not found: {path}")),
            None => Err(format!("No file path for Rekordbox track {source_id}")),
        };
        match imported {
            Ok((track_id, is_new)) => {
                if let Err(error) = guard.checkpoint() {
                    if is_new {
                        new_ids.push(track_id);
                    }
                    return Err(rollback_after_failure(
                        services,
                        &new_ids,
                        principal.as_deref(),
                        error,
                    )
                    .await);
                }
                if is_new {
                    new_ids.push(track_id.clone());
                }
                if let Some(track) =
                    crate::database::local::tracks::get_track_by_id(&services.db.0, &track_id)
                        .await?
                {
                    push_unique(&mut tracks, track);
                }
                emit_import_progress(
                    services,
                    &import_id,
                    source,
                    TrackImportPhase::Importing,
                    done + 1,
                    total,
                    Some(track_id),
                    None,
                    None,
                );
            }
            Err(message) => {
                emit_import_progress(
                    services,
                    &import_id,
                    source,
                    TrackImportPhase::Importing,
                    done + 1,
                    total,
                    None,
                    None,
                    Some(message.clone()),
                );
                failures.push(TrackImportFailure { source_id, message });
            }
        }
    }
    finish_fast_import(
        services,
        epoch,
        guard,
        lease,
        &import_id,
        source,
        total,
        new_ids,
        principal.as_deref(),
    )
    .await?;
    Ok(TrackImportResult {
        import_id,
        tracks,
        failures,
    })
}
