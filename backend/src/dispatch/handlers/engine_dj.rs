//! Read-only access to an Engine DJ (Denon) library.
//!
//! `library_path` is the Engine Library *root folder*; `open_engine_db` appends
//! `Database2/m.db`. Each command opens a read-only, single-connection pool and
//! closes it again, so Luma never holds a lock on the user's library.
//!
use std::future::Future;

use sqlx::SqlitePool;

use crate::dispatch::{AppServices, CommandError};
use crate::engine_dj;
use crate::engine_dj::types::{EngineDjLibraryInfo, EngineDjPlaylist, EngineDjTrack};
use crate::models::tracks::{TrackImportFailure, TrackImportResult};
use crate::services::tracks::{ImportRequest, LibraryTrack};

use super::tracks::importer;

/// Open the library, run one read against it, close it.
///
/// The close runs on the failure path too, so a failed read never leaves the
/// pool holding a lock on the user's library until drop.
async fn with_library<T, F, Fut>(library_path: &str, read: F) -> Result<T, CommandError>
where
    F: FnOnce(SqlitePool) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let pool = engine_dj::db::open_engine_db(library_path).await?;
    let result = read(pool.clone()).await;
    pool.close().await;
    result.map_err(CommandError::Internal)
}

/// Library identity and size. Both callers use its error as an existence probe
/// before offering a folder picker.
pub async fn engine_dj_open_library(
    _services: &AppServices,
    library_path: String,
) -> Result<EngineDjLibraryInfo, CommandError> {
    // `get_library_info` echoes the path back in its result, so this is the one
    // read that needs it inside the closure as well as outside.
    let echoed = library_path.clone();
    with_library(&library_path, |pool| async move {
        engine_dj::db::get_library_info(&pool, &echoed).await
    })
    .await
}

/// Flat playlist list ordered by title; `parent_id` back-references let the
/// caller assemble the tree.
pub async fn engine_dj_list_playlists(
    _services: &AppServices,
    library_path: String,
) -> Result<Vec<EngineDjPlaylist>, CommandError> {
    with_library(&library_path, |pool| async move {
        engine_dj::db::list_playlists(&pool).await
    })
    .await
}

/// The whole library, unpaged.
pub async fn engine_dj_list_tracks(
    _services: &AppServices,
    library_path: String,
) -> Result<Vec<EngineDjTrack>, CommandError> {
    with_library(&library_path, |pool| async move {
        engine_dj::db::list_tracks(&pool).await
    })
    .await
}

/// One playlist's tracks, ordered by title rather than by the DJ's manual
/// order, and non-recursive: child playlists are not included.
pub async fn engine_dj_get_playlist_tracks(
    _services: &AppServices,
    library_path: String,
    playlist_id: i64,
) -> Result<Vec<EngineDjTrack>, CommandError> {
    with_library(&library_path, |pool| async move {
        engine_dj::db::get_playlist_tracks(&pool, playlist_id).await
    })
    .await
}

/// Substring search across title/artist/filename over the whole library,
/// silently capped at 200 rows by the query.
pub async fn engine_dj_search_tracks(
    _services: &AppServices,
    library_path: String,
    query: String,
) -> Result<Vec<EngineDjTrack>, CommandError> {
    with_library(&library_path, |pool| async move {
        engine_dj::db::search_tracks(&pool, &query).await
    })
    .await
}

/// Where an Engine Library lives by default. Pure path construction — it does
/// not check that anything is there, so callers probe with
/// [`engine_dj_open_library`].
pub async fn engine_dj_default_library_path(
    _services: &AppServices,
) -> Result<String, CommandError> {
    Ok(engine_dj::default_library_path()
        .to_string_lossy()
        .to_string())
}

pub async fn engine_dj_import_tracks(
    services: &AppServices,
    library_path: String,
    track_ids: Vec<i64>,
) -> Result<TrackImportResult, CommandError> {
    let principal = services.require_session().await?;
    let (database_uuid, rows) = services.track_sources.engine_tracks(&library_path).await?;
    let requests = track_ids.into_iter().map(|id| {
        let row = rows
            .iter()
            .find(|row| row.id == id)
            .ok_or_else(|| TrackImportFailure {
                source_id: id.to_string(),
                message: format!("Engine DJ track {id} not found"),
            })?;
        Ok(ImportRequest::Library(LibraryTrack {
            source_id: format!("{database_uuid}:{}", row.id),
            title: row.title.clone(),
            artist: row.artist.clone(),
            album: row.album.clone(),
            duration_seconds: row.length,
            filename: Some(row.filename.clone()),
            audio_path: Ok(engine_dj::resolve_engine_path(&library_path, &row.path)),
        }))
    });
    Ok(importer(services)
        .import(principal, "engine_dj", requests.collect())
        .await?)
}
