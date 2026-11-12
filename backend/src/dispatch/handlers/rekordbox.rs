//! Read-only access to the user's Rekordbox library.
//!
//! Every one of these reopens the SQLCipher `master.db` from scratch on a
//! blocking thread — `rekordbox::subprocess` keeps no cached handle — so they
//! are all `spawn_blocking` wrappers and nothing more.
//!
use std::path::PathBuf;

use crate::dispatch::AppServices;
use crate::error::CommandError;
use crate::models::tracks::{ImportRejection, TrackImportFailure, TrackImportResult};
use crate::rekordbox::subprocess;
use crate::rekordbox::types::{RekordboxLibraryInfo, RekordboxPlaylist, RekordboxTrack};
use crate::services::tracks::{ImportRequest, LibraryTrack};

use super::tracks::importer;

/// Run a Rekordbox DB read off the async runtime.
///
/// A panic in `rbox` surfaces as a join error rather than taking the host down,
/// which is why every command below goes through here.
pub(crate) async fn blocking<T, F>(read: F) -> Result<T, CommandError>
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
    let principal = services.require_session().await?;
    let rows = services.track_sources.rekordbox_tracks().await?;
    let requests = track_uuids.into_iter().map(|uuid| {
        let Some(row) = rows.iter().find(|row| row.uuid == uuid) else {
            return Err(TrackImportFailure {
                message: format!("Rekordbox track with UUID {uuid} not found"),
                source_id: uuid,
                name: None,
                reason: ImportRejection::Missing,
            });
        };
        Ok(ImportRequest::Library(LibraryTrack {
            audio_path: row
                .file_path
                .as_ref()
                .map(PathBuf::from)
                .ok_or_else(|| format!("No file path for Rekordbox track {uuid}")),
            source_id: uuid,
            title: row.title.clone(),
            artist: row.artist.clone(),
            album: row.album.clone(),
            filename: row.filename.clone(),
        }))
    });
    Ok(importer(services)
        .import(principal, "rekordbox", requests.collect())
        .await?)
}
