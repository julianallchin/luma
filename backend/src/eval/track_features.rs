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
    /// Per band, its lowest and highest energy over the whole track.
    ranges: std::sync::Mutex<std::collections::HashMap<[u64; 2], (f64, f64)>>,
}

/// How often [`TrackFeatures::range`] samples the track, in seconds.
const RANGE_STEP: f64 = 0.05;

impl TrackFeatures {
    fn band(&self, request: &FeatureRequest, seconds: f64) -> p::Result<f64> {
        crate::audio::spectrum::sample_band(
            &self.mix,
            SAMPLE_RATE,
            seconds,
            [request.low_hz as f32, request.high_hz as f32],
        )
        .map(f64::from)
        .map_err(p::Error)
    }
}

impl p::FeatureSource for TrackFeatures {
    fn sample(&self, request: &FeatureRequest, beat: f64) -> p::Result<f64> {
        self.band(request, self.clock.seconds_at(beat)?)
    }

    fn range(&self, request: &FeatureRequest) -> p::Result<(f64, f64)> {
        let key = [request.low_hz.to_bits(), request.high_hz.to_bits()];
        if let Some(range) = self.ranges.lock().expect("ranges lock").get(&key) {
            return Ok(*range);
        }
        let length = self.mix.len() as f64 / f64::from(SAMPLE_RATE);
        let steps = (length / RANGE_STEP).ceil() as usize;
        let mut range = (f64::INFINITY, f64::NEG_INFINITY);
        for i in 0..steps.max(1) {
            let value = self.band(request, i as f64 * RANGE_STEP)?;
            range = (range.0.min(value), range.1.max(value));
        }
        self.ranges.lock().expect("ranges lock").insert(key, range);
        Ok(range)
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
        ranges: Default::default(),
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
            ranges: Default::default(),
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

    #[test]
    fn range_spans_the_band_over_the_whole_track() {
        // A tone that is silent for the first half second, then loud.
        let samples: Vec<f32> = (0..SAMPLE_RATE)
            .map(|i| {
                let loud = if i < SAMPLE_RATE / 2 { 0. } else { 1. };
                loud * (std::f32::consts::TAU * 100. * i as f32 / SAMPLE_RATE as f32).sin()
            })
            .collect();
        let features = TrackFeatures {
            clock: p::BeatTimeline::new(vec![0., 0.5, 1., 1.5], 0.).unwrap(),
            mix: Arc::new(samples),
            ranges: Default::default(),
        };
        let request = FeatureRequest {
            low_hz: 50.,
            high_hz: 150.,
        };
        let (low, high) = features.range(&request).unwrap();
        assert_eq!(low, 0.);
        for beat in [0.5, 1.2, 1.6, 1.9] {
            let value = features.sample(&request, beat).unwrap();
            assert!(
                value >= low && value <= high * 1.05,
                "{value} in {low}–{high}"
            );
        }
        assert!(high >= features.sample(&request, 1.9).unwrap() * 0.95);
        assert_eq!(features.range(&request).unwrap(), (low, high));
    }
}
