use std::collections::HashMap;

use serde::Serialize;

use crate::database::local::tracks as tracks_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::models::node_graph::BeatGrid;
use crate::models::tracks::{TrackBrowserRow, TrackSummary};
use crate::services::tracks::{self as track_service, TrackBarClassifications};

/// The whole library, unscoped and without album-art bytes.
pub async fn list_tracks(services: &AppServices) -> Result<Vec<TrackSummary>, CommandError> {
    Ok(track_service::list_tracks(&services.db.0).await?)
}

/// Clip counts for one venue, keyed by track id. Sparse — see
/// [`tracks_db::get_venue_annotation_counts`].
pub async fn get_venue_annotation_counts(
    services: &AppServices,
    venue_id: String,
) -> Result<HashMap<String, i64>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(tracks_db::get_venue_annotation_counts(&mut access).await?)
}

/// `None` when beat detection has not run for this track.
pub async fn get_track_beats(
    services: &AppServices,
    track_id: String,
) -> Result<Option<BeatGrid>, CommandError> {
    Ok(track_service::get_track_beats(&services.db.0, &track_id).await?)
}

/// `None` when bar classification has not run. Both fields are opaque JSON —
/// there is no Rust-side schema for the classification shape.
pub async fn get_track_bar_classifications(
    services: &AppServices,
    track_id: String,
) -> Result<Option<TrackBarClassifications>, CommandError> {
    Ok(track_service::get_track_bar_classifications(&services.db.0, &track_id).await?)
}

/// Per-class drum onset timestamps (seconds), keyed by the n2n class names
/// `kick`, `snare`, `hat`, `cymbal`. `None` when transcription has not run, and
/// individual classes may still be absent.
pub async fn get_track_drum_onsets(
    services: &AppServices,
    track_id: String,
) -> Result<Option<HashMap<String, Vec<f32>>>, CommandError> {
    Ok(tracks_db::get_track_drum_onsets(&services.db.0, &track_id).await?)
}

/// Per-tag F1-optimal suggestion thresholds bundled with the classifier
/// weights, as `tag_name -> threshold`. Callers use these in place of a flat
/// 0.5 cutoff so rare tags (e.g. `vocal_chop` at 0.165) surface at the
/// calibration the model was tuned for.
pub async fn get_classifier_thresholds(
    _services: &AppServices,
) -> Result<HashMap<String, f64>, CommandError> {
    Ok(track_service::classifier_thresholds()?)
}

/// Delete a track's row and its files. A hard delete, not an archive; the
/// `sync_delete_tracks` trigger enqueues the committed row deletion for push.
pub async fn delete_track(services: &AppServices, track_id: String) -> Result<(), CommandError> {
    let principal = services.session_user_id().await?;
    track_service::delete_track(
        &services.db.0,
        &services.storage,
        &services.stem_cache,
        &track_id,
        principal.as_deref(),
    )
    .await?;
    Ok(())
}

/// A track's audio as standard base64 plus a MIME type inferred from the file
/// extension.
///
/// Only justified because the bytes are fed to an LLM as a file part — this
/// reads the whole file into memory and crosses the wire as a string. Playback
/// must not use it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackAudioBase64 {
    pub data: String,
    pub mime_type: String,
}

pub async fn get_track_audio_base64(
    services: &AppServices,
    track_id: String,
) -> Result<TrackAudioBase64, CommandError> {
    let (data, mime_type) =
        track_service::get_track_audio_base64(&services.db.0, &track_id).await?;
    Ok(TrackAudioBase64 { data, mime_type })
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
