//! Host-level audio playback: load a track or a segment of one, transport
//! control, and a priming read of the transport state.
//!
//! Times crossing this boundary are **segment-relative** once loaded: the host
//! remembers the segment's absolute start, so `host_seek` and the `currentTime`
//! in a snapshot are offsets from it, not absolute track time. `host_load_track`
//! loads from 0.0, which is why the two coincide in the track editor.
//!
//! The steady-state channel for transport state is the `host-audio://state`
//! event; `host_snapshot` exists only to fill the gap before the first
//! broadcast arrives.

use std::path::Path;

use crate::database::local::track_access::{Operate, Read, VisibleTrackAccess};
use crate::dispatch::{AppServices, CommandError};
use crate::host_audio::{device_sample_rate, HostAudioSnapshot};
use crate::models::node_graph::BeatGrid;

/// Load a slice of a track for playback. `start_time` and `end_time` are
/// absolute track seconds; `end_time <= 0` means "to the end".
pub async fn host_load_segment(
    services: &AppServices,
    track_id: String,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
) -> Result<(), CommandError> {
    let pool = &services.db.0;
    let mut access = VisibleTrackAccess::<Read>::read(pool, &track_id).await?;
    let admitted_principal = access.principal().map(str::to_owned);
    let info = crate::database::local::tracks::get_track_path_and_hash_for_connection(
        access.connection(),
        &track_id,
    )
    .await
    .map_err(|e| CommandError::Internal(format!("Failed to fetch track: {}", e)))?;
    drop(access);

    let audio = decode(&info.file_path, &info.track_hash)?;

    // Frame indices, then sample indices — the buffer is stereo interleaved.
    let num_frames = audio.samples.len() / 2;
    let start_frame = (start_time * audio.sample_rate as f32).floor().max(0.0) as usize;
    let end_frame = if end_time > 0.0 {
        (end_time * audio.sample_rate as f32).ceil() as usize
    } else {
        num_frames
    };

    let samples = if start_frame >= num_frames {
        Vec::new()
    } else {
        let capped_end_frame = end_frame.min(num_frames);
        audio.samples[start_frame * 2..capped_end_frame * 2].to_vec()
    };

    if samples.is_empty() {
        return Err(CommandError::Invalid(
            "Segment time range produced empty audio".into(),
        ));
    }

    commit_segment(
        services,
        &track_id,
        admitted_principal.as_deref(),
        samples,
        audio.sample_rate,
        beat_grid,
        start_time,
    )
    .await
}

/// Load a whole track for playback, with its beat grid if one is stored.
/// Segment start is 0.0, so snapshot times equal absolute track times.
pub async fn host_load_track(services: &AppServices, track_id: String) -> Result<(), CommandError> {
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

    let beat_grid =
        crate::services::tracks::get_track_beats_for_connection(access.connection(), &track_id)
            .await
            .ok()
            .flatten();
    drop(access);

    commit_segment(
        services,
        &track_id,
        admitted_principal.as_deref(),
        audio.samples.clone(),
        audio.sample_rate,
        beat_grid,
        0.0,
    )
    .await
}

/// Decode through the shared audio cache, so later analysis reuses this decode.
fn decode(
    file_path: &str,
    track_hash: &str,
) -> Result<std::sync::Arc<crate::audio::decoder::DecodedAudio>, CommandError> {
    let audio = crate::audio::load_or_decode_audio_shared(
        Path::new(file_path),
        track_hash,
        device_sample_rate(),
    )
    .map_err(|e| CommandError::Internal(format!("Failed to decode track: {}", e)))?;
    if audio.samples.is_empty() || audio.sample_rate == 0 {
        return Err(CommandError::Invalid("Track has no audio data".into()));
    }
    Ok(audio)
}

/// Install `samples` on the host under a fresh `Operate` lease, refusing if the
/// admitted identity changed while the (slow) decode was running.
#[allow(clippy::too_many_arguments)]
async fn commit_segment(
    services: &AppServices,
    track_id: &str,
    admitted_principal: Option<&str>,
    samples: Vec<f32>,
    sample_rate: u32,
    beat_grid: Option<BeatGrid>,
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
        .load_segment(samples, sample_rate, beat_grid, segment_start)?;
    Ok(access.commit().await?)
}

/// Start playback. Errors when nothing is loaded; does not seek first.
pub async fn host_play(services: &AppServices) -> Result<(), CommandError> {
    Ok(services.host_audio.play()?)
}

/// Pause playback. A no-op when nothing is loaded.
pub async fn host_pause(services: &AppServices) -> Result<(), CommandError> {
    services.host_audio.pause();
    Ok(())
}

/// Seek to `seconds` relative to the loaded segment's start.
pub async fn host_seek(services: &AppServices, seconds: f32) -> Result<(), CommandError> {
    Ok(services.host_audio.seek(seconds)?)
}

/// Enable or disable looping over the current loop region.
pub async fn host_set_loop(services: &AppServices, enabled: bool) -> Result<(), CommandError> {
    services.host_audio.set_loop(enabled);
    Ok(())
}

/// Set the loop region in segment-relative seconds.
///
/// Looping follows the region: it turns on only when both bounds are given, so
/// clearing either bound is how the caller turns looping off.
pub async fn host_set_loop_region(
    services: &AppServices,
    start_seconds: Option<f32>,
    end_seconds: Option<f32>,
) -> Result<(), CommandError> {
    let enabled = start_seconds.is_some() && end_seconds.is_some();
    services
        .host_audio
        .set_loop_region(start_seconds, end_seconds);
    services.host_audio.set_loop(enabled);
    Ok(())
}

/// Set playback rate (1.0 = normal). Changes pitch; no time-stretch.
pub async fn host_set_playback_rate(services: &AppServices, rate: f32) -> Result<(), CommandError> {
    services.host_audio.set_playback_rate(rate);
    Ok(())
}

/// Read transport state once. The `host-audio://state` event carries the same
/// payload continuously; this is the priming read before the first one lands.
pub async fn host_snapshot(services: &AppServices) -> Result<HostAudioSnapshot, CommandError> {
    Ok(services.host_audio.snapshot())
}
