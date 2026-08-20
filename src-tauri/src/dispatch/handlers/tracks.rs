use crate::database::local::tracks as tracks_db;
use crate::dispatch::{AppServices, CommandError};
use crate::models::tracks::TrackBrowserRow;
use crate::services::tracks as track_service;

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

/// The write is committed before the event goes out, so a lost `library-changed`
/// is not a failed rename and must not be reported as one.
pub async fn update_track_metadata(
    services: &AppServices,
    track_id: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
) -> Result<(), CommandError> {
    tracks_db::update_track_metadata(
        &services.db.0,
        &track_id,
        title.as_deref(),
        artist.as_deref(),
        album.as_deref(),
    )
    .await?;
    services.events.emit("library-changed", ());
    Ok(())
}
