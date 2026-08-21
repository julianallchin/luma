use crate::dispatch::{AppServices, CommandError};
use crate::models::waveforms::{TrackWaveform, WaveformWindow};
use crate::services::waveforms as waveform_service;

pub async fn get_track_waveform(
    services: &AppServices,
    track_id: String,
) -> Result<TrackWaveform, CommandError> {
    Ok(
        waveform_service::get_track_waveform(&services.db.0, &services.analysis_tasks, &track_id)
            .await?,
    )
}

/// One visible range at one pixel density. Unlike [`get_track_waveform`] this
/// is a *view* of the audio rather than a stored artifact, so it takes no
/// analysis lease: nothing it does can be published, and two callers asking for
/// overlapping ranges are two reads of the same decoded buffer.
pub async fn get_track_waveform_window(
    services: &AppServices,
    track_id: String,
    start_seconds: f64,
    end_seconds: f64,
    buckets: u32,
) -> Result<WaveformWindow, CommandError> {
    Ok(waveform_service::get_track_waveform_window(
        &services.db.0,
        &track_id,
        start_seconds,
        end_seconds,
        buckets,
    )
    .await?)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use serde_json::json;

    use crate::database::local::{auth, database, state};
    use crate::dispatch::{dispatch, AppServices};
    use crate::models::waveforms::{TrackWaveform, WaveformWindow};
    use crate::services::waveforms::FULL_WAVEFORM_SIZE;

    /// Long enough that the stored envelope is *starved*: 30 000 buckets over
    /// ninety seconds is one every 3 ms, and a timeline at 500 px/s wants one
    /// every 2 ms. Below this length there is nothing for a window to add.
    const SECONDS: u32 = 90;
    const RATE: u32 = 48_000;
    /// The gate's period. A 3 ms stored bucket always contains one whole open
    /// gate, so the stored envelope reads as full-scale everywhere; a 1 ms
    /// bucket does not, and that difference is the detail under test.
    const GATE_MS: f64 = 2.;

    /// The range the window is asked for, in seconds, and how finely.
    const WINDOW: (f64, f64) = (30., 33.);
    const FINE_BUCKETS: u32 = 3_000;

    #[tokio::test]
    async fn a_zoomed_window_resolves_detail_the_stored_envelope_smeared_away() {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;

        let fine: WaveformWindow = window(&services, FINE_BUCKETS).await;
        assert_eq!(fine.min.len(), FINE_BUCKETS as usize);
        assert_eq!(fine.max.len(), FINE_BUCKETS as usize);
        assert_eq!(fine.rms.len(), FINE_BUCKETS as usize);
        assert!((fine.start_seconds - WINDOW.0).abs() < 1e-6);
        assert!((fine.end_seconds - WINDOW.1).abs() < 1e-6);

        // The stored envelope itself, over the same range: what a renderer
        // stretching `get_track_waveform` across these pixels has to draw with.
        let stored: TrackWaveform = serde_json::from_value(
            dispatch(
                &services,
                "get_track_waveform",
                &json!({ "trackId": "track" }),
            )
            .await
            .expect("the stored envelope failed to compute"),
        )
        .unwrap();
        let full = stored.full_samples.expect("a computed waveform has one");
        assert_eq!(full.len(), FULL_WAVEFORM_SIZE * 2);
        let per_second = FULL_WAVEFORM_SIZE as f64 / stored.duration_seconds;
        let coarse: Vec<f32> = ((WINDOW.0 * per_second) as usize..(WINDOW.1 * per_second) as usize)
            .map(|bucket| full[bucket * 2 + 1])
            .collect();
        assert!(
            (fine.max.len() as f64 / coarse.len() as f64) > 2.,
            "the window is not meaningfully finer than the stored envelope here"
        );

        // Detail is *troughs*: the gate closes for a millisecond at a time, so
        // the fine buckets keep falling to nothing and the stored ones — each
        // spanning a whole gate cycle — never do. No interpolation of the
        // stored series can produce a trough that is not in it.
        let quiet = |maxima: &[f32]| {
            maxima.iter().filter(|v| **v < 0.3).count() as f64 / maxima.len() as f64
        };
        let (fine_quiet, coarse_quiet) = (quiet(&fine.max), quiet(&coarse));
        assert!(
            fine_quiet > 0.2,
            "the fine window never fell quiet ({fine_quiet:.3}); it is not resolving the gate"
        );
        assert!(
            coarse_quiet < 0.01,
            "the stored envelope's density already resolves the gate ({coarse_quiet:.3}); \
             this track is not a test of anything"
        );

        // RMS is the same buckets, not a second query's worth of them.
        assert!(fine.rms.iter().all(|value| (0. ..=1.).contains(value)));
        assert!(fine.rms.iter().any(|value| *value > 0.1));
    }

    #[tokio::test]
    async fn a_window_past_either_end_is_clamped_rather_than_refused() {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;

        let value = dispatch(
            &services,
            "get_track_waveform_window",
            &json!({
                "trackId": "track",
                "startSeconds": -10.,
                "endSeconds": f64::from(SECONDS) + 10.,
                "buckets": 64,
            }),
        )
        .await
        .expect("a window past the ends is answered, not refused");
        let window: WaveformWindow = serde_json::from_value(value).unwrap();
        assert_eq!(window.start_seconds, 0.);
        assert!(window.end_seconds <= f64::from(SECONDS) + 0.001);
        assert_eq!(window.max.len(), 64);
    }

    async fn window(services: &AppServices, buckets: u32) -> WaveformWindow {
        let value = dispatch(
            services,
            "get_track_waveform_window",
            &json!({
                "trackId": "track",
                "startSeconds": WINDOW.0,
                "endSeconds": WINDOW.1,
                "buckets": buckets,
            }),
        )
        .await
        .expect("the window command failed");
        serde_json::from_value(value).unwrap()
    }

    /// One track, with real audio behind it — the command measures the file, so
    /// a row alone would have nothing to measure.
    async fn seed(directory: &Path) -> AppServices {
        let audio = directory.join("gated.wav");
        std::fs::write(&audio, wav()).unwrap();

        let db = database::init_app_db_at(directory).await.unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, title, duration_seconds, file_path)
             VALUES ('track', NULL, 'gated-hash', 'Gated', ?, ?)",
        )
        .bind(f64::from(SECONDS))
        .bind(audio.to_string_lossy().to_string())
        .execute(&db.0)
        .await
        .unwrap();

        let state_db = state::init_state_db_at(directory).await.unwrap();
        auth::bootstrap_host_admission(&db.0, &state_db.0)
            .await
            .unwrap();
        let storage = crate::storage::StorageRoot::from_path(directory.to_path_buf());
        let workspaces = Arc::new(
            crate::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("no Python here".to_string())),
            ),
        );
        AppServices::headless(db, state_db, storage, directory.to_path_buf(), workspaces)
    }

    /// A 4 kHz tone gated on and off every [`GATE_MS`] / 2, as 16-bit stereo
    /// WAV at the rate the audio host decodes to — so the decode that answers
    /// the command is not also a resample, which would soften the gate's edges
    /// and make the assertion about the resampler.
    fn wav() -> Vec<u8> {
        let frames = RATE * SECONDS;
        let period = f64::from(RATE) * GATE_MS / 1000.;
        let mut samples = Vec::with_capacity(frames as usize * 4);
        for frame in 0..frames {
            let open = (f64::from(frame) % period) < period / 2.;
            let t = f64::from(frame) / f64::from(RATE);
            let value = if open {
                ((t * 4000. * std::f64::consts::TAU).sin() * 0.98 * f64::from(i16::MAX)) as i16
            } else {
                0
            };
            samples.extend_from_slice(&value.to_le_bytes());
            samples.extend_from_slice(&value.to_le_bytes());
        }

        let mut file = Vec::with_capacity(samples.len() + 44);
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
        file.extend_from_slice(b"WAVEfmt ");
        file.extend_from_slice(&16u32.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&2u16.to_le_bytes());
        file.extend_from_slice(&RATE.to_le_bytes());
        file.extend_from_slice(&(RATE * 4).to_le_bytes());
        file.extend_from_slice(&4u16.to_le_bytes());
        file.extend_from_slice(&16u16.to_le_bytes());
        file.extend_from_slice(b"data");
        file.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        file.extend_from_slice(&samples);
        file
    }
}

/// `get_track_waveform` minus the cache lookup: both funnel into
/// `ensure_track_waveform` under the same analysis lease.
pub async fn reprocess_waveform(
    services: &AppServices,
    track_id: String,
) -> Result<TrackWaveform, CommandError> {
    Ok(waveform_service::reprocess_track_waveform(
        &services.db.0,
        &services.analysis_tasks,
        &track_id,
    )
    .await?)
}
