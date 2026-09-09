//! Wheel zoom uses the shared spring in log scale, retaining velocity across notches.
use super::View;
use luma_ui::motion;
use std::time::Instant;

pub(super) struct Zoom {
    value: f32,
    target: f32,
    velocity: f32,
    at: Instant,
}
impl Zoom {
    pub fn new(zoom: f32, now: Instant) -> Self {
        Self {
            value: zoom.ln(),
            target: zoom.ln(),
            velocity: 0.,
            at: now,
        }
    }
    pub fn push(&mut self, delta: f32) {
        self.target = (self.target + delta).clamp(View::MIN_ZOOM.ln(), View::MAX_ZOOM.ln());
    }
    pub fn advance(&mut self, now: Instant) -> (f32, bool) {
        let dt = now.saturating_duration_since(self.at).as_secs_f32();
        self.at = now;
        let (displacement, velocity) = motion::ROOT.advance(
            self.value - self.target,
            self.velocity,
            dt,
            motion::SNAP as f32 / 1000.,
        );
        self.value = (self.target + displacement).clamp(View::MIN_ZOOM.ln(), View::MAX_ZOOM.ln());
        self.velocity = velocity;
        let settled = (self.value - self.target).abs() < 0.00005 && self.velocity.abs() < 0.001;
        if settled {
            self.value = self.target;
            self.velocity = 0.;
        }
        (
            self.value.exp().clamp(View::MIN_ZOOM, View::MAX_ZOOM),
            settled,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn notches_accumulate_and_retarget_without_jumping() {
        let now = Instant::now();
        let mut zoom = Zoom::new(100., now);
        zoom.push(0.4);
        let (first, _) = zoom.advance(now + Duration::from_millis(16));
        assert!(first > 100. && first < 100. * 0.4f32.exp());
        let velocity = zoom.velocity;
        zoom.push(0.4);
        assert_eq!(zoom.velocity, velocity);
        assert_eq!(zoom.advance(now + Duration::from_millis(16)).0, first);
        let (end, settled) = zoom.advance(now + Duration::from_secs(1));
        assert!(settled);
        assert!((end - 100. * 0.8f32.exp()).abs() < 0.001);
    }
    #[test]
    fn reversing_and_limits_settle_without_drift() {
        let now = Instant::now();
        let mut zoom = Zoom::new(100., now);
        zoom.push(0.5);
        zoom.advance(now + Duration::from_millis(25));
        zoom.push(-0.5);
        let (end, settled) = zoom.advance(now + Duration::from_secs(1));
        assert!(settled && (end - 100.).abs() < 0.001);
        zoom.push(100.);
        assert!((zoom.advance(now + Duration::from_secs(2)).0 - View::MAX_ZOOM).abs() < 0.01);
        zoom.push(-200.);
        assert!((zoom.advance(now + Duration::from_secs(3)).0 - View::MIN_ZOOM).abs() < 0.001);
    }
}
