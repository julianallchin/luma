use crate::dispatch::{AppServices, CommandError};
use crate::models::waveforms::{TrackWaveform, WaveformWindow};
use crate::services::waveforms as waveform_service;

pub async fn get_track_waveform(
    services: &AppServices,
    track_id: String,
) -> Result<TrackWaveform, CommandError> {
    Ok(
        waveform_service::get_track_waveform(&services.db.0, &services.analysis_tasks, &track_id)
            .await?
            .0,
    )
}

/// One visible range at one pixel density, in the stored envelope's own band
/// units — the same three series [`get_track_waveform`] returns, over a shorter
/// range and a finer grid.
///
/// Two callers asking for overlapping ranges are two reads of the same decoded
/// buffer. It takes an analysis lease only for the one case it cannot answer
/// from a read: a waveform row written before the band gains were stored has no
/// units to answer in, and materialising them is a publication.
pub async fn get_track_waveform_window(
    services: &AppServices,
    track_id: String,
    start_seconds: f64,
    end_seconds: f64,
    buckets: u32,
) -> Result<WaveformWindow, CommandError> {
    Ok(waveform_service::get_track_waveform_window(
        &services.db.0,
        &services.analysis_tasks,
        &track_id,
        start_seconds,
        end_seconds,
        buckets,
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
    use crate::models::waveforms::{BandEnvelopes, TrackWaveform, WaveformWindow};
    use crate::services::waveforms::FULL_WAVEFORM_SIZE;

    /// Long enough that the stored envelope is *starved*: 30 000 buckets over
    /// ninety seconds is one every 3 ms, and a timeline at 500 px/s wants one
    /// every 2 ms. Below this length there is nothing for a window to add.
    const SECONDS: u32 = 90;
    const RATE: u32 = 48_000;
    /// The gate: 1 ms of carrier then 2 ms of silence, over and over.
    ///
    /// Both halves of the detail test live in those two numbers. A 3 ms stored
    /// bucket spans a whole period, so it always contains the open millisecond
    /// and reads full-scale; a 1 ms window bucket lands wholly inside the
    /// silence one time in three, and that trough is detail no interpolation of
    /// the stored series can invent.
    const GATE_OPEN_MS: f64 = 1.;
    const GATE_PERIOD_MS: f64 = 3.;

    /// The range the window is asked for, in seconds, and how finely.
    const WINDOW: (f64, f64) = (30., 33.);
    const FINE_BUCKETS: u32 = 3_000;

    /// A quiet stretch of the track, and how far down it is — [`WINDOW`] sits
    /// inside it.
    ///
    /// This is what makes the parity test mean something. Normalisation is
    /// against a *whole-track* percentile, which the loud rest of the track
    /// sets; a window that normalised against itself instead would find its own
    /// peak here and draw this quarter-height passage at full scale. On a track
    /// of even loudness the two answers coincide and the test proves nothing,
    /// so the fixture makes them disagree by a mile.
    const QUIET: (f64, f64) = (25., 45.);
    const QUIET_LEVEL: f64 = 0.3;

    #[tokio::test]
    async fn a_zoomed_window_resolves_detail_the_stored_envelope_smeared_away() {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;

        let fine: WaveformWindow = window(&services, FINE_BUCKETS).await;
        assert_eq!(fine.bands.low.len(), FINE_BUCKETS as usize);
        assert_eq!(fine.bands.mid.len(), FINE_BUCKETS as usize);
        assert_eq!(fine.bands.high.len(), FINE_BUCKETS as usize);
        assert!((fine.start_seconds - WINDOW.0).abs() < 1e-6);
        assert!((fine.end_seconds - WINDOW.1).abs() < 1e-6);

        // The gate rides the high band — see [`wav`] — so that is the band the
        // two resolutions can disagree about.
        let coarse = stored_band(&services, |bands| bands.high.clone()).await;
        assert!(
            (fine.bands.high.len() as f64 / coarse.len() as f64) > 2.,
            "the window is not meaningfully finer than the stored envelope here"
        );

        // Detail is *troughs*: the gate closes for two milliseconds at a time,
        // so the fine buckets keep falling to nothing and the stored ones —
        // each spanning a whole gate cycle — never do. No interpolation of the
        // stored series can produce a trough that is not in it.
        let quiet =
            |band: &[f32]| band.iter().filter(|v| **v < 0.2).count() as f64 / band.len() as f64;
        let (fine_quiet, coarse_quiet) = (quiet(&fine.bands.high), quiet(&coarse));
        assert!(
            fine_quiet > 0.2,
            "the fine window never fell quiet ({fine_quiet:.3}); it is not resolving the gate"
        );
        assert!(
            coarse_quiet < 0.01,
            "the stored envelope's density already resolves the gate ({coarse_quiet:.3}); \
             this track is not a test of anything"
        );
    }

    /// The parity the whole design turns on: at the stored envelope's own
    /// density, a measured window *is* the stored envelope. If these two ever
    /// drift apart, the editor's picture changes when it crosses the zoom
    /// threshold — which is the bug this seam exists to make impossible.
    ///
    /// The window is cut over [`QUIET`], a passage at [`QUIET_LEVEL`] of the
    /// track's loudness, so "same units" is a claim with teeth: a window
    /// normalised against its own range instead of the track's would draw this
    /// passage at full scale, which is the wrong answer by more than half the
    /// strip and nowhere near the tolerances below.
    #[tokio::test]
    async fn a_window_at_the_stored_density_is_the_stored_envelope() {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;
        assert!(
            WINDOW.0 >= QUIET.0 && WINDOW.1 <= QUIET.1,
            "the window has to sit inside the quiet passage for this to test units"
        );

        // The stored envelope over the same range, and a window cut to exactly
        // as many buckets as it has there.
        for take in [
            (|b: &BandEnvelopes| b.low.clone()) as fn(&BandEnvelopes) -> Vec<f32>,
            |b| b.mid.clone(),
            |b| b.high.clone(),
        ] {
            let coarse = stored_band(&services, take).await;
            let measured = window(&services, coarse.len() as u32).await;
            let measured = take(&measured.bands);
            assert_eq!(measured.len(), coarse.len());

            // Not bit-equal, and it cannot be: the stored envelope is measured
            // off a native-rate decode and a window off the device-rate one the
            // audio host keeps, so on a machine whose output device disagrees
            // with the file the window is measuring resampled audio on a bucket
            // grid a sample or two off. That is a real few-percent wobble in
            // every bar and it is not a defect.
            //
            // The bound is in the unit that matters. A band draws at most
            // `half` pixels tall — around 36 on this strip — so 0.05 of full
            // scale is under two pixels of bar height, and 0.15 at the worst
            // bar is five. Getting the units wrong is not a near miss: it is a
            // band drawn three times its height.
            let (drift, worst) = measured.iter().zip(&coarse).fold(
                (0.0f32, 0.0f32),
                |(sum, worst), (fine, stored)| {
                    let error = (fine - stored).abs();
                    (sum + error, worst.max(error))
                },
            );
            let mean = drift / measured.len() as f32;
            assert!(
                mean < 0.05 && worst < 0.15,
                "a window at the stored density is a different picture \
                 (mean {mean:.4}, worst {worst:.4})"
            );
        }
    }

    /// The fixture is only a test of units while the quiet passage is actually
    /// quiet in the stored envelope. If a change to the signal or to the
    /// compression ever floats it back up to full scale, the parity test above
    /// goes quietly green against nothing.
    #[tokio::test]
    async fn the_quiet_passage_is_visibly_quiet_in_the_stored_envelope() {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;

        let quiet = stored_band(&services, |bands| bands.low.clone()).await;
        let loudest = quiet.iter().fold(0.0f32, |peak, value| peak.max(*value));
        // `Band::Low`'s ceiling is 0.95; full scale here would be that.
        assert!(
            loudest < 0.7,
            "the quiet passage draws at {loudest:.3}, near the low band's 0.95 \
             ceiling — a window normalised against itself would look the same, \
             so the parity test is not testing units"
        );
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
        assert_eq!(window.bands.low.len(), 64);
    }

    /// One band of the stored envelope over [`WINDOW`], which is what a
    /// renderer draws either side of the threshold.
    async fn stored_band(services: &AppServices, take: fn(&BandEnvelopes) -> Vec<f32>) -> Vec<f32> {
        let stored: TrackWaveform = serde_json::from_value(
            dispatch(
                services,
                "get_track_waveform",
                &json!({ "trackId": "track" }),
            )
            .await
            .expect("the stored envelope failed to compute"),
        )
        .unwrap();
        let bands = stored
            .bands
            .expect("a computed waveform has band envelopes");
        assert_eq!(bands.low.len(), FULL_WAVEFORM_SIZE);
        let per_second = FULL_WAVEFORM_SIZE as f64 / stored.duration_seconds;
        take(&bands)[(WINDOW.0 * per_second) as usize..(WINDOW.1 * per_second) as usize].to_vec()
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
