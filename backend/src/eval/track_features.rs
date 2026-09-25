//! Track data for canonical graphs: the full mix, loaded once under the
//! score's authorized snapshot. All frame sampling is pure and seekable.
use crate::{
    audio::SAMPLE_RATE, database::local::venue_access::AuthorizedVenue, storage::StorageRoot,
};
use luma_patterns::{self as p, FeatureRequest};

#[derive(Debug)]
pub(crate) struct TrackFeatures {
    clock: p::BeatTimeline,
    mix: super::ResidentAudio,
}

impl p::FeatureSource for TrackFeatures {
    fn sample(&self, request: &FeatureRequest, beat: f64) -> p::Result<f64> {
        crate::audio::spectrum::sample_band(
            &self.mix,
            SAMPLE_RATE,
            self.clock.seconds_at(beat)?,
            [request.low_hz as f32, request.high_hz as f32],
        )
        .map(f64::from)
        .map_err(p::Error)
    }
}

pub(crate) async fn prepare(
    access: &mut impl AuthorizedVenue,
    storage: &StorageRoot,
    track_id: &str,
    grid: &crate::models::node_graph::BeatGrid,
    requests: &[FeatureRequest],
) -> Result<std::sync::Arc<TrackFeatures>, String> {
    let path = crate::database::local::tracks::get_track_path_and_hash_for_connection(
        access.connection(),
        track_id,
    )
    .await?;
    let mix = super::context::load_track_audio_cached(storage, &path.file_path, &path.track_hash)?;
    if mix.is_empty() {
        return Err("mix audio is empty".into());
    }
    let nyquist = f64::from(SAMPLE_RATE / 2);
    if requests.iter().any(|request| request.high_hz > nyquist) {
        return Err(format!(
            "frequency band exceeds the mix's Nyquist frequency ({nyquist} Hz)"
        ));
    }
    Ok(std::sync::Arc::new(TrackFeatures {
        clock: grid.timeline().map_err(|e| e.to_string())?,
        mix,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use p::FeatureSource;
    use std::sync::Arc;

    #[test]
    fn band_energy_reads_the_mix_at_the_beat_and_is_silent_outside_it() {
        let samples: Vec<f32> = (0..SAMPLE_RATE)
            .map(|i| (std::f32::consts::TAU * 100. * i as f32 / SAMPLE_RATE as f32).sin())
            .collect();
        let features = TrackFeatures {
            clock: p::BeatTimeline::new(vec![0., 0.5, 1., 1.5], 0.).unwrap(),
            mix: Arc::new(samples.clone()),
        };
        let request = FeatureRequest {
            low_hz: 50.,
            high_hz: 150.,
        };
        let energy = features.sample(&request, 1.6).unwrap();
        let expected =
            crate::audio::spectrum::sample_band(&samples, SAMPLE_RATE, 0.8, [50., 150.]).unwrap();
        assert_eq!(energy, f64::from(expected));
        assert!(energy > 0.);
        assert_eq!(features.sample(&request, 3.).unwrap(), 0.);
    }
}
