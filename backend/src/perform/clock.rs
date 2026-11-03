//! A deck's playhead between reports.
//!
//! DJ sources report a deck's position a few to a few dozen times a second,
//! and the lights are sampled far more often than that. [`DeckClock`] is the
//! extrapolation the render engine samples; [`DeckFollower`] turns a stream of
//! position reports into one, smoothing arrival jitter and inferring pause for
//! sources that only report a position when it moves.

use std::time::{Duration, Instant};

/// Corrections larger than this are seeks, loops or a cue jump: snap to them.
const SNAP: f64 = 0.020;
/// Fraction of a small error taken per report. Small errors are mostly packet
/// arrival jitter, so they are filtered rather than followed.
const SLEW: f64 = 0.25;
/// A source that only reports moving positions has paused once it has been
/// this quiet.
pub(crate) const PAUSE_AFTER: Duration = Duration::from_millis(150);

/// Where a deck's playhead is at any instant: `position` seconds at `at`,
/// advancing at `rate` while playing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeckClock {
    position: f64,
    at: Instant,
    rate: f64,
    playing: bool,
}

impl DeckClock {
    pub fn stopped(position: f64, at: Instant) -> Self {
        Self {
            position,
            at,
            rate: 1.0,
            playing: false,
        }
    }

    pub fn running(position: f64, at: Instant, rate: f64) -> Self {
        Self {
            position,
            at,
            rate,
            playing: true,
        }
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    /// The playhead at `now`, in track seconds.
    pub fn position_at(&self, now: Instant) -> f64 {
        if !self.playing {
            return self.position;
        }
        // `now` can precede the anchor when a reader took its instant before a
        // report landed; run the line backwards rather than clamp, so the
        // answer stays continuous.
        let elapsed = if now >= self.at {
            (now - self.at).as_secs_f64()
        } else {
            -(self.at - now).as_secs_f64()
        };
        self.position + elapsed * self.rate
    }
}

/// Builds a [`DeckClock`] from position reports.
#[derive(Clone, Debug, Default)]
pub struct DeckFollower {
    clock: Option<DeckClock>,
    /// The last reported position and when it first arrived. A repeat of the
    /// same position is not a new observation: sources resend whole snapshots
    /// when any one field changes.
    last: Option<(f64, Instant)>,
    /// Whether play state is inferred from movement rather than reported.
    inferred: bool,
}

impl DeckFollower {
    pub fn clock(&self) -> Option<DeckClock> {
        self.clock
    }

    /// Take one report. `playing` is `None` for sources that never say, which
    /// are then playing while their position keeps changing.
    pub fn observe(&mut self, position: f64, rate: f64, playing: Option<bool>, at: Instant) {
        let rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };
        let previous = self.last;
        let moved = previous.is_none_or(|(last, _)| last != position);
        if moved {
            self.last = Some((position, at));
        }
        self.inferred = playing.is_none();
        let was_playing = self.clock.is_some_and(|clock| clock.playing);
        // Inferred play needs two moves close together, so a single cue jump
        // on a paused deck does not set it running.
        let playing = playing.unwrap_or_else(|| {
            if moved {
                was_playing || previous.is_some_and(|(_, seen)| at - seen <= PAUSE_AFTER)
            } else {
                was_playing
            }
        });

        let (position, seen) = self.last.expect("a report was recorded");
        self.clock = Some(match self.clock {
            _ if !playing => DeckClock::stopped(position, seen),
            Some(clock) if clock.playing && moved => {
                let predicted = clock.position_at(at);
                let error = position - predicted;
                if error.abs() > SNAP {
                    DeckClock::running(position, at, rate)
                } else {
                    DeckClock::running(predicted + error * SLEW, at, rate)
                }
            }
            // Still playing, no new position: only the rate can have changed.
            Some(clock) if clock.playing => DeckClock::running(clock.position_at(at), at, rate),
            _ => DeckClock::running(position, at, rate),
        });
    }

    /// Stop an inferred deck that has gone quiet. Returns whether it stopped.
    pub fn tick(&mut self, now: Instant) -> bool {
        let Some((position, seen)) = self.last else {
            return false;
        };
        let stale = now.saturating_duration_since(seen) > PAUSE_AFTER;
        if self.inferred && stale && self.clock.is_some_and(|clock| clock.playing) {
            self.clock = Some(DeckClock::stopped(position, seen));
            return true;
        }
        false
    }

    /// Forget everything: a new track was loaded.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Feed `count` inferred reports, `step` apart, of a deck playing at `rate`.
    fn play(
        follower: &mut DeckFollower,
        start: Instant,
        from: f64,
        rate: f64,
        step: Duration,
        count: u32,
    ) -> Instant {
        let mut at = start;
        for i in 0..count {
            at = start + step * i;
            follower.observe(from + (step * i).as_secs_f64() * rate, rate, None, at);
        }
        at
    }

    #[test]
    fn a_playing_deck_runs_on_between_reports() {
        let t0 = Instant::now();
        let mut follower = DeckFollower::default();
        let last = play(&mut follower, t0, 10.0, 1.0, ms(16), 10);
        let clock = follower.clock().unwrap();
        let reported = 10.0 + (last - t0).as_secs_f64();
        let later = clock.position_at(last + ms(10));
        assert!(
            later > reported,
            "the playhead advances past the last report"
        );
        assert!((later - (reported + 0.010)).abs() < 0.002);
    }

    #[test]
    fn pitch_sets_the_rate() {
        let t0 = Instant::now();
        let mut follower = DeckFollower::default();
        let last = play(&mut follower, t0, 0.0, 1.08, ms(16), 10);
        let clock = follower.clock().unwrap();
        let step = clock.position_at(last + ms(100)) - clock.position_at(last);
        assert!(step > 0.100, "a pitched-up deck runs faster than wall time");
    }

    #[test]
    fn one_move_on_a_paused_deck_is_a_cue_not_play() {
        let t0 = Instant::now();
        let mut follower = DeckFollower::default();
        follower.observe(30.0, 1.0, None, t0);
        follower.observe(64.0, 1.0, None, t0 + ms(2000));
        let clock = follower.clock().unwrap();
        assert!(!clock.playing());
        assert_eq!(clock.position_at(t0 + ms(2500)), 64.0);
    }

    #[test]
    fn an_inferred_deck_pauses_when_reports_stop() {
        let t0 = Instant::now();
        let mut follower = DeckFollower::default();
        let last = play(&mut follower, t0, 5.0, 1.0, ms(16), 8);
        assert!(!follower.tick(last + PAUSE_AFTER / 2));
        assert!(follower.tick(last + PAUSE_AFTER * 2));
        let clock = follower.clock().unwrap();
        let held = clock.position_at(last + ms(5000));
        assert!(!clock.playing());
        assert_eq!(
            held,
            5.0 + (last - t0).as_secs_f64(),
            "paused at the last report"
        );
    }

    #[test]
    fn a_reported_pause_holds_regardless_of_quiet() {
        let t0 = Instant::now();
        let mut follower = DeckFollower::default();
        follower.observe(1.0, 1.0, Some(true), t0);
        follower.observe(1.2, 1.0, Some(true), t0 + ms(200));
        assert!(
            !follower.tick(t0 + ms(5000)),
            "explicit sources say when they pause"
        );
        follower.observe(1.2, 1.0, Some(false), t0 + ms(5000));
        assert!(!follower.clock().unwrap().playing());
    }

    #[test]
    fn a_seek_snaps_and_jitter_does_not() {
        let t0 = Instant::now();
        let mut follower = DeckFollower::default();
        let last = play(&mut follower, t0, 0.0, 1.0, ms(16), 10);
        let expected = (last - t0).as_secs_f64();

        // A late packet: 5 ms of arrival jitter moves the playhead by less.
        let late = last + ms(21);
        follower.observe(expected + 0.016, 1.0, None, late);
        let jittered = follower.clock().unwrap().position_at(late);
        let truth = expected + 0.021;
        assert!((jittered - truth).abs() < 0.005);

        // A jump far ahead is taken whole.
        let seek = late + ms(16);
        follower.observe(120.0, 1.0, None, seek);
        assert_eq!(follower.clock().unwrap().position_at(seek), 120.0);
    }

    #[test]
    fn repeated_snapshots_are_not_new_observations() {
        let t0 = Instant::now();
        let mut follower = DeckFollower::default();
        follower.observe(2.0, 1.0, Some(true), t0);
        // Another field changed; the snapshot repeats the same position later.
        follower.observe(2.0, 1.0, Some(true), t0 + ms(100));
        let at = follower.clock().unwrap().position_at(t0 + ms(100));
        assert!(
            (at - 2.1).abs() < 1e-9,
            "the repeat must not drag the playhead back"
        );
    }
}
