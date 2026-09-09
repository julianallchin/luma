//! Display-rate motion disciplined by the audio host's less frequent readings.
use std::time::Instant;

#[derive(Default)]
pub(super) struct Clock {
    sample: Option<(Instant, f64)>,
    frame: Option<(Instant, f64)>,
}

impl Clock {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn observe(&mut self, now: Instant, position: f64, playing: bool) {
        if !playing {
            self.reset();
            return;
        }
        // A seek or loop wrap must jump immediately. Small polling/dispatch
        // jitter is corrected gradually rather than moving the picture backwards.
        if self.sample.is_none_or(|(at, old)| {
            (old + now.duration_since(at).as_secs_f64() - position).abs() > 0.1
        }) {
            self.frame = Some((now, position));
        }
        self.sample = Some((now, position));
    }

    pub fn position(&mut self, now: Instant) -> Option<f64> {
        let (sample_at, sample) = self.sample?;
        let (frame_at, previous) = self.frame?;
        let dt = now.duration_since(frame_at).as_secs_f64();
        let target = sample + now.duration_since(sample_at).as_secs_f64();
        let predicted = previous + dt;
        let position = predicted + (target - predicted) * (1. - (-dt / 0.1).exp());
        self.frame = Some((now, position));
        Some(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn moves_every_display_frame_despite_thirty_hz_polling() {
        let start = Instant::now();
        let mut clock = Clock::default();
        clock.observe(start, 10., true);
        let mut previous = 10.;
        for frame in 1..=120 {
            let t = frame as f64 / 120.;
            let now = start + Duration::from_secs_f64(t);
            if frame % 4 == 0 {
                // Alternating dispatch delay should not produce a visible poll step.
                clock.observe(now, 10. + t - if frame % 8 == 0 { 0.002 } else { 0. }, true);
            }
            let position = clock.position(now).unwrap();
            assert!((position - previous - 1. / 120.).abs() < 0.0002);
            previous = position;
        }
        clock.observe(start + Duration::from_secs(2), 3., true);
        assert_eq!(clock.position(start + Duration::from_secs(2)), Some(3.));
        clock.observe(start + Duration::from_secs(3), 4., false);
        assert_eq!(clock.position(start + Duration::from_secs(4)), None);
    }
}
