use crate::database::local::tracks as tracks_db;
use crate::dispatch::{AppServices, CommandError};

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
