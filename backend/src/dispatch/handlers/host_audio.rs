//! Host-level audio playback: load a track, transport control, and a priming
//! read of the transport state.
//!
//! Times crossing this boundary are **segment-relative** once loaded: the host
//! remembers the segment's absolute start, so `host_seek` and the `currentTime`
//! in a snapshot are offsets from it, not absolute track time. `host_load_track`
//! loads from 0.0, which is why the two coincide in the track editor.
//!
//! The native editor polls `host_snapshot`. Every control and snapshot names
//! the playback session, so delayed work cannot operate on another song.

use std::path::Path;

use crate::audio::SAMPLE_RATE;
use crate::database::local::track_access::{Operate, Read, VisibleTrackAccess};
use crate::dispatch::{AppServices, CommandError};
use crate::host_audio::{HostAudioControl, HostAudioSnapshot};

/// Load a whole track for playback.
/// Segment start is 0.0, so snapshot times equal absolute track times.
pub async fn host_load_track(
    services: &AppServices,
    track_id: String,
    session: u64,
) -> Result<(), CommandError> {
    if !services.host_audio.begin_load(session, track_id.clone()) {
        return Ok(());
    }
    services
        .track_audio
        .ensure(&track_id)
        .await
        .map_err(|error| CommandError::Internal(error.to_string()))?;
    let pool = &services.db.0;
    let mut access = VisibleTrackAccess::<Read>::read(pool, &track_id).await?;
    let admitted_principal = access.principal().map(str::to_owned);
    let info = crate::database::local::tracks::get_track_path_and_hash_for_connection(
        access.connection(),
        &track_id,
    )
    .await
    .map_err(|e| CommandError::Internal(format!("Failed to fetch track: {}", e)))?;

    let audio = decode(&info.file_path, &info.track_hash)?;

    drop(access);

    commit_segment(
        services,
        &track_id,
        admitted_principal.as_deref(),
        session,
        audio.to_vec(),
        0.0,
    )
    .await
}

/// What a file preview put on the host: how long the file is, and a coarse
/// loudness overview of it to draw a scrub bar from.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePreview {
    pub duration_seconds: f32,
    /// [`PREVIEW_OVERVIEW_BUCKETS`] peaks in `0..=1`, start to end.
    pub overview: Vec<f32>,
}

/// Enough peaks for a scrub bar a text column wide to read as a waveform.
pub const PREVIEW_OVERVIEW_BUCKETS: usize = 160;

/// Load an audio file that is not a library track — a DJ library's row being
/// auditioned — onto the one host player, as `session`.
///
/// The same player the track editor uses, so a preview and a song cannot play
/// over each other: loading this unloads whatever was playing. `Ok(None)`
/// means a newer session took the player before this one finished decoding.
///
/// The overview is computed from the samples just decoded for playback — the
/// same peaks the library's waveforms are built from — rather than by a
/// second pass over the file.
pub async fn host_load_file(
    services: &AppServices,
    path: String,
    session: u64,
) -> Result<Option<FilePreview>, CommandError> {
    if !services
        .host_audio
        .begin_load(session, format!("file:{path}"))
    {
        return Ok(None);
    }
    let audio =
        tokio::task::spawn_blocking(move || crate::audio::decode_file_shared(Path::new(&path)))
            .await
            .map_err(|error| {
                CommandError::Internal(format!("preview decode task failed: {error}"))
            })?
            .map_err(|error| CommandError::Internal(format!("Failed to decode file: {error}")))?;
    if audio.is_empty() {
        return Err(CommandError::Invalid("File has no audio data".into()));
    }
    let overview = crate::services::waveforms::compute_waveform(&audio, PREVIEW_OVERVIEW_BUCKETS)
        .chunks_exact(2)
        .map(|pair| pair[0].abs().max(pair[1].abs()).min(1.0))
        .collect();
    let duration_seconds = (audio.len() / 2) as f32 / SAMPLE_RATE as f32;
    services
        .host_audio
        .finish_load(session, audio.to_vec(), 0.0)?;
    Ok(Some(FilePreview {
        duration_seconds,
        overview,
    }))
}

/// Release `session`'s audio if it still owns the player.
pub async fn host_release(services: &AppServices, session: u64) -> Result<(), CommandError> {
    services.host_audio.release(session);
    Ok(())
}

/// Decode through the shared audio cache, so later analysis reuses this decode.
fn decode(file_path: &str, track_hash: &str) -> Result<std::sync::Arc<Vec<f32>>, CommandError> {
    let audio = crate::audio::load_or_decode_audio_shared(Path::new(file_path), track_hash)
        .map_err(|e| CommandError::Internal(format!("Failed to decode track: {}", e)))?;
    if audio.is_empty() {
        return Err(CommandError::Invalid("Track has no audio data".into()));
    }
    Ok(audio)
}

/// Install `samples` on the host under a fresh `Operate` lease, refusing if the
/// admitted identity changed while the (slow) decode was running.
async fn commit_segment(
    services: &AppServices,
    track_id: &str,
    admitted_principal: Option<&str>,
    session: u64,
    samples: Vec<f32>,
    segment_start: f32,
) -> Result<(), CommandError> {
    let access = VisibleTrackAccess::<Operate>::operate(&services.db.0, track_id).await?;
    if access.principal() != admitted_principal {
        return Err(CommandError::Unauthorized(
            "authenticated identity changed while loading audio".into(),
        ));
    }
    services
        .host_audio
        .finish_load(session, samples, segment_start)?;
    Ok(access.commit().await?)
}

/// Seek and start as one operation owned by this playback session.
pub async fn host_play(
    services: &AppServices,
    session: u64,
    seconds: f32,
) -> Result<(), CommandError> {
    Ok(services
        .host_audio
        .control(session, HostAudioControl::Play { seconds })?)
}

pub async fn host_pause(services: &AppServices, session: u64) -> Result<(), CommandError> {
    Ok(services
        .host_audio
        .control(session, HostAudioControl::Pause)?)
}

pub async fn host_seek(
    services: &AppServices,
    session: u64,
    seconds: f32,
) -> Result<(), CommandError> {
    Ok(services
        .host_audio
        .control(session, HostAudioControl::Seek { seconds })?)
}

pub async fn host_set_loop_region(
    services: &AppServices,
    session: u64,
    start_seconds: Option<f32>,
    end_seconds: Option<f32>,
) -> Result<(), CommandError> {
    Ok(services.host_audio.control(
        session,
        HostAudioControl::Loop {
            start: start_seconds,
            end: end_seconds,
        },
    )?)
}

/// Read the current session and its transport state for the native editor.
pub async fn host_snapshot(services: &AppServices) -> Result<HostAudioSnapshot, CommandError> {
    Ok(services.host_audio.snapshot())
}
