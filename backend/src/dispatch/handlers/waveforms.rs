use crate::dispatch::{AppServices, CommandError};
use crate::models::waveforms::{TrackWaveform, WaveformSignal};
use crate::services::waveforms as waveform_service;

pub async fn get_track_waveform(
    services: &AppServices,
    track_id: String,
) -> Result<TrackWaveform, CommandError> {
    services
        .sync
        .ensure_track_audio(&services.storage, &track_id)
        .await
        .map_err(|error| CommandError::Internal(error.to_string()))?;
    Ok(
        waveform_service::get_track_waveform(&services.db.0, &services.analysis_tasks, &track_id)
            .await?
            .0,
    )
}

/// Typed native dispatch: avoids serializing tens of millions of samples.
pub async fn get_track_waveform_signal(
    services: &AppServices,
    track_id: String,
) -> Result<WaveformSignal, CommandError> {
    services
        .sync
        .ensure_track_audio(&services.storage, &track_id)
        .await
        .map_err(|error| CommandError::Internal(error.to_string()))?;
    Ok(waveform_service::get_track_waveform_signal(
        &services.db.0,
        &services.analysis_tasks,
        &track_id,
        services.host_audio.decode_sample_rate(),
    )
    .await?)
}

/// `get_track_waveform` minus the cache lookup: both funnel into
/// `ensure_track_waveform` under the same analysis lease.
pub async fn reprocess_waveform(
    services: &AppServices,
    track_id: String,
) -> Result<TrackWaveform, CommandError> {
    services
        .sync
        .ensure_track_audio(&services.storage, &track_id)
        .await
        .map_err(|error| CommandError::Internal(error.to_string()))?;
    Ok(waveform_service::reprocess_track_waveform(
        &services.db.0,
        &services.analysis_tasks,
        &track_id,
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
    use crate::models::waveforms::TrackWaveform;
    use crate::services::waveforms::FULL_WAVEFORM_SIZE;

    const SECONDS: u32 = 90;
    const RATE: u32 = 48_000;
    const GATE_PERIOD_MS: f64 = 3.;
    const GATE_OPEN_MS: f64 = 1.;
    const QUIET: (f64, f64) = (25., 45.);
    const QUIET_LEVEL: f64 = 0.3;

    #[tokio::test]
    async fn native_signal_preserves_sample_rate_and_stored_normalization() {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;
        let signal = super::get_track_waveform_signal(&services, "track".into())
            .await
            .unwrap();
        assert_eq!(signal.sample_rate, RATE);
        for band in &signal.bands {
            assert_eq!(band.len(), (RATE * SECONDS) as usize);
            assert!(band.iter().all(|v| v.is_finite()));
        }
        let stored: TrackWaveform = serde_json::from_value(
            dispatch(&services, "get_track_waveform", &json!({"trackId":"track"}))
                .await
                .unwrap(),
        )
        .unwrap();
        let filtered = crate::audio::FilteredBands {
            low: signal.bands[0].clone(),
            mid: signal.bands[1].clone(),
            high: signal.bands[2].clone(),
        };
        let peaks = super::waveform_service::bucketize_band_peaks(
            &filtered,
            0..filtered.low.len(),
            FULL_WAVEFORM_SIZE,
        );
        let measured = signal.gains.compress(&peaks);
        let stored = stored.bands.unwrap();
        for (a, b) in [
            (measured.low, stored.low),
            (measured.mid, stored.mid),
            (measured.high, stored.high),
        ] {
            assert!(a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 0.001));
        }
    }

    #[tokio::test]
    async fn native_signal_refuses_a_missing_track() {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;
        assert!(
            super::get_track_waveform_signal(&services, "missing".into())
                .await
                .is_err()
        );
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
        auth::bootstrap_headless_admission(&db.0, &state_db.0)
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

    /// A steady 100 Hz tone under a gated 10 kHz carrier, as 16-bit stereo WAV
    /// at the rate the audio host decodes to — so the decode that answers the
    /// command is not also a resample, which would soften the gate's edges and
    /// make the assertion about the resampler.
    ///
    /// Two tones because the thing under test is a *band* envelope. A gate
    /// faster than a filter's impulse response is invisible to that filter, so
    /// the gate rides the high band (a 4 kHz highpass settles in a fraction of
    /// a millisecond) while the low band carries steady content that gives the
    /// whole-track percentile something real to normalise against. The carrier
    /// is a whole number of cycles per gate edge, so the gate opens and closes
    /// on a zero crossing and adds no click for the other bands to hear.
    ///
    /// All of it drops to [`QUIET_LEVEL`] across [`QUIET`] — see that constant
    /// for why a track of even loudness cannot test normalisation at all.
    fn wav() -> Vec<u8> {
        let frames = RATE * SECONDS;
        let period = f64::from(RATE) * GATE_PERIOD_MS / 1000.;
        let open = f64::from(RATE) * GATE_OPEN_MS / 1000.;
        let mut samples = Vec::with_capacity(frames as usize * 4);
        for frame in 0..frames {
            let t = f64::from(frame) / f64::from(RATE);
            let bass = (t * 100. * std::f64::consts::TAU).sin() * 0.5;
            let carrier = if (f64::from(frame) % period) < open {
                (t * 10_000. * std::f64::consts::TAU).sin() * 0.45
            } else {
                0.
            };
            let level = if (QUIET.0..QUIET.1).contains(&t) {
                QUIET_LEVEL
            } else {
                1.
            };
            let value = ((bass + carrier) * level * f64::from(i16::MAX)) as i16;
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
