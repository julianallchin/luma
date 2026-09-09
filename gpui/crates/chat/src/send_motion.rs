//! New-turn room and the message's flight from the composer. Layout owns the
//! destination; the shared live spring follows it without losing velocity.
use std::collections::VecDeque;
use std::time::Instant;

use gpui::SharedString;
use luma_ui::motion;

pub const PEEK: f32 = 48.0;
/// Give the new turn more time to rise while its message is in flight.
pub const SCROLL_RATE: f32 = 0.65;

pub struct Pending {
    pub id: SharedString,
    pub text: String,
}

pub struct Flight {
    pub id: SharedString,
    pub text: String,
    pub y: f32,
    velocity: f32,
    tick: Instant,
}

impl Flight {
    pub fn step(&mut self, target: f32, reduced: bool) -> bool {
        let now = Instant::now();
        let seconds = now.duration_since(self.tick).as_secs_f32().min(0.05);
        self.tick = now;
        let (y, velocity) = advance(self.y, self.velocity, target, seconds, reduced);
        self.y = y;
        self.velocity = velocity;
        (y - target).abs() < 0.5 && velocity.abs() < 2.0
    }
}

fn advance(y: f32, velocity: f32, target: f32, seconds: f32, reduced: bool) -> (f32, f32) {
    if reduced {
        return (target, 0.0);
    }
    let (offset, velocity) = motion::ROOT.advance(
        y - target,
        velocity,
        seconds,
        motion::span(&motion::SURFACE).as_secs_f32(),
    );
    // Keep the shared spring's acceleration, but settle at the destination
    // instead of crossing it and rebounding as the scroll comes to rest.
    let next = target + offset;
    let bounded = next.clamp(y.min(target), y.max(target));
    (bounded, if bounded == next { velocity } else { 0.0 })
}

#[derive(Default)]
pub struct SendMotion {
    pub pending: VecDeque<Pending>,
    pub flight: Option<Flight>,
    pub anchor: Option<SharedString>,
    pub anchor_offset: Option<f32>,
    pub room: f32,
    pub rendered_pending: usize,
    sequence: u64,
}

impl SendMotion {
    pub fn start(&mut self, text: String, origin: f32, reduced: bool) {
        self.sequence += 1;
        let id: SharedString = format!("pending-send-{}", self.sequence).into();
        self.anchor = Some(id.clone());
        self.anchor_offset = None;
        self.flight = (!reduced).then(|| Flight {
            id: id.clone(),
            text: text.clone(),
            y: origin,
            velocity: 0.0,
            tick: Instant::now(),
        });
        self.pending.push_back(Pending { id, text });
    }

    /// Transfer the presentation identity when the backend accepts a prompt.
    pub fn accept(&mut self, id: &str) {
        let Some(pending) = self.pending.pop_front() else {
            return;
        };
        if self.anchor.as_ref() == Some(&pending.id) {
            self.anchor = Some(id.to_string().into());
        }
        if let Some(flight) = &mut self.flight {
            if flight.id == pending.id {
                flight.id = id.to_string().into();
            }
        }
    }
}

/// Reserve only the space the current exchange has not filled yet.
pub fn room(viewport: f32, turn_height: f32) -> f32 {
    (viewport - PEEK - turn_height).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_consumes_reserved_room_before_growing_the_scroll_range() {
        assert_eq!(room(800.0, 100.0), 652.0);
        assert_eq!(room(800.0, 500.0), 252.0);
        assert_eq!(room(800.0, 900.0), 0.0);
    }

    #[test]
    fn flight_preserves_velocity_as_the_destination_scrolls() {
        let (mut y, mut velocity) = (700.0, 0.0);
        for frame in 0..120 {
            let target = 500.0 - (frame as f32 * 12.0).min(440.0);
            (y, velocity) = advance(y, velocity, target, 1.0 / 60.0, false);
            assert!(y.is_finite() && velocity.is_finite());
            assert!(
                y >= target,
                "the message must not overshoot its destination"
            );
        }
        assert!((y - 60.0).abs() < 0.5);
        assert_eq!(advance(y, velocity, 40.0, 0.016, true), (40.0, 0.0));
    }

    #[test]
    fn rapid_sends_keep_the_latest_anchor_when_earlier_prompts_arrive() {
        let mut state = SendMotion::default();
        state.start("first".into(), 700.0, false);
        state.start("second".into(), 700.0, false);
        state.accept("first-durable");
        assert_eq!(state.anchor.as_deref(), Some("pending-send-2"));
        state.accept("second-durable");
        assert_eq!(state.anchor.as_deref(), Some("second-durable"));
        assert_eq!(state.flight.as_ref().unwrap().id, "second-durable");
    }
}
