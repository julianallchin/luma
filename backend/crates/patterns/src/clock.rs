use crate::{Error, Result};

/// Piecewise musical time from detected beats. Supports tempo changes and
/// extrapolates using the edge interval; average BPM is never substituted.
#[derive(Clone, Debug)]
pub struct BeatTimeline {
    seconds: Vec<f64>,
    origin: f64,
}
impl BeatTimeline {
    /// Detected beat positions in absolute seconds, independent of the musical
    /// origin used by beat_at/seconds_at. Useful for aligned preview overlays.
    pub fn timestamps(&self) -> &[f64] {
        &self.seconds
    }

    pub fn new(seconds: Vec<f64>, origin_seconds: f64) -> Result<Self> {
        if seconds.len() < 2
            || seconds.iter().any(|v| !v.is_finite())
            || seconds.windows(2).any(|w| w[0] >= w[1])
            || !origin_seconds.is_finite()
        {
            return Err(Error(
                "beat grid needs at least two finite, strictly increasing timestamps".into(),
            ));
        }
        let mut timeline = Self {
            seconds,
            origin: 0.0,
        };
        timeline.origin = timeline.raw_beat(origin_seconds);
        Ok(timeline)
    }
    pub fn beat_at(&self, seconds: f64) -> Result<f64> {
        if !seconds.is_finite() {
            return Err(Error("time must be finite".into()));
        }
        Ok(self.raw_beat(seconds) - self.origin)
    }
    /// Inverse of beat_at, including pickups and either end's extrapolation.
    pub fn seconds_at(&self, beat: f64) -> Result<f64> {
        if !beat.is_finite() {
            return Err(Error("beat position must be finite".into()));
        }
        let raw = beat + self.origin;
        let left = raw.floor().max(0.0).min((self.seconds.len() - 2) as f64) as usize;
        let seconds = self.seconds[left]
            + (raw - left as f64) * (self.seconds[left + 1] - self.seconds[left]);
        if !seconds.is_finite() {
            return Err(Error("beat position exceeds the timeline range".into()));
        }
        Ok(seconds)
    }
    fn raw_beat(&self, seconds: f64) -> f64 {
        let next = self.seconds.partition_point(|t| *t <= seconds);
        let left = next.saturating_sub(1).min(self.seconds.len() - 2);
        left as f64 + (seconds - self.seconds[left]) / (self.seconds[left + 1] - self.seconds[left])
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn variable_tempo_and_nonzero_origin() {
        let clock = BeatTimeline::new(vec![1.0, 1.5, 2.0, 3.0, 4.0], 1.5).unwrap();
        assert_eq!(clock.beat_at(1.5).unwrap(), 0.0);
        assert_eq!(clock.beat_at(1.75).unwrap(), 0.5);
        assert_eq!(clock.beat_at(2.5).unwrap(), 1.5);
        assert_eq!(clock.beat_at(5.0).unwrap(), 4.0);
        assert_eq!(clock.beat_at(0.5).unwrap(), -2.0);
        assert!(BeatTimeline::new(vec![1.0, 1.0], 0.0).is_err());
        for seconds in [-5.0, 0.5, 1.0, 1.75, 2.5, 3.25, 4.0, 10.0] {
            assert!(
                (clock.seconds_at(clock.beat_at(seconds).unwrap()).unwrap() - seconds).abs()
                    < 1e-12
            );
        }
    }
}
