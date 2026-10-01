//! New-turn room and the message's flight from the composer. Layout owns the
//! destination; the shared live spring follows it without losing velocity.
use std::collections::VecDeque;
use std::time::Instant;

use gpui::SharedString;
use luma_ui::motion;

pub const PEEK: f32 = 48.0;
/// How much faster than a streaming follow the transcript rises under a sent
/// message. At 1.5 the stick spring covers 90% of the way in about 250 ms and
/// lands in about 680 ms — Comet's own-send glide (90% in ~230 ms). Adapted
/// from Comet, MIT, (c) 2026 Wing.
pub const SCROLL_RATE: f32 = 1.5;
/// The message's flight from the composer: the quick rung, so the prompt is
/// in place about as soon as the transcript has made room for it.
const FLIGHT: motion::MotionSpec = motion::MotionSpec::new(motion::QUICK, motion::ROOT);

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
        motion::span(&FLIGHT).as_secs_f32(),
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
    /// [`Self::room`] before it is floored at zero, and the height of the
    /// tool card folding in this exchange when it was measured, if one was —
    /// see [`Self::room_with_fold`].
    slack: f32,
    slack_fold: Option<f32>,
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

    /// Measure the room from a finished layout: what the viewport has left
    /// under the current exchange. `fold` is the height of the tool card
    /// folding in this exchange in that layout, if one is.
    pub fn measure(&mut self, viewport: f32, turn_height: f32, fold: Option<f32>) -> f32 {
        self.slack = viewport - PEEK - turn_height;
        self.slack_fold = fold;
        room(viewport, turn_height)
    }

    /// The room for the layout being made, given the folding card's height
    /// in it.
    ///
    /// [`Self::measure`] reads a finished layout, so on its own the room
    /// answers a fold one frame late: a card that shrinks first shortens the
    /// list, which pulls the view down, and only then does the room grow
    /// back. Taking the card's change since the measure keeps the exchange
    /// and its room one height in the same layout. Before a measure has seen
    /// the fold, this is the measured room.
    #[must_use]
    pub fn room_with_fold(&self, fold: f32) -> f32 {
        match self.slack_fold {
            Some(measured) => (self.slack - (fold - measured)).max(0.0),
            None => self.room,
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

    /// While a card folds, the exchange and its room keep one height: what
    /// the card gives up the room takes, and what it grows the room gives, in
    /// the same layout and not one frame later.
    #[test]
    fn a_folding_card_trades_height_with_the_room() {
        let mut state = SendMotion::default();
        let open = 120.0;
        state.room = state.measure(800.0, 300.0 + open, None);
        // The fold has not been measured yet: the room is the measured one.
        assert_eq!(state.room_with_fold(0.0), state.room);
        let room = state.measure(800.0, 300.0 + open, Some(open));
        assert_eq!(state.room_with_fold(open), room);
        for fold in [90.0, 40.0, 0.0] {
            assert_eq!(state.room_with_fold(fold) + fold, room + open);
        }
        // A card that grows past the room's slack cannot make it negative.
        let tight = state.measure(800.0, 700.0, Some(0.0));
        assert!(state.room_with_fold(200.0) >= 0.0);
        assert!(state.room_with_fold(200.0) < tight);
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

    /// The transcript rises under a sent message about as fast as the message
    /// flies: most of the rise is done within the flight's span, so the
    /// message does not hang at its landing waiting for the scroll.
    #[test]
    fn the_rise_keeps_pace_with_the_flight() {
        let mut spring = crate::transcript::StickSpring::default();
        let rise = 600.0;
        let frames = FLIGHT.total().as_secs_f32() * 1000.0 / crate::theme::SPRING_FRAME_MS;
        let mut pos = 0.0;
        for _ in 0..frames.ceil() as usize {
            pos = spring.step(pos, rise, SCROLL_RATE);
        }
        assert!(
            pos >= 0.9 * rise,
            "only {pos}px of {rise}px risen when the message lands"
        );
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
