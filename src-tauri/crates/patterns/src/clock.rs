use crate::{Error, Result};

/// Piecewise musical time from detected beats. Supports tempo changes and
/// extrapolates using the edge interval; average BPM is never substituted.
#[derive(Clone, Debug)]
pub struct BeatTimeline {
    seconds: Vec<f64>,
    origin: f64,
}
impl BeatTimeline {
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
    }
}
