//! Preview motor lag: the stage shows what a real head can do.
//!
//! The score and the DMX output are never slowed. Only the drawn beam turns
//! toward the solver's pan and tilt at a motor's speed. See
//! `docs/specs/aim.md`, "Preview motor lag".
use std::collections::HashMap;
use std::time::Instant;

use luma_lib::models::universe::UniverseState;

/// How fast a previewed head turns on each axis. Fixture profiles carry no
/// motor speed, so every head gets this one.
const DEGREES_PER_SECOND: f32 = 180.0;

/// Track time that runs ahead of the wall clock by more than this, or back
/// by more than [`SEEK_BACK_S`], is a seek.
const SEEK_AHEAD_S: f32 = 0.25;
const SEEK_BACK_S: f32 = 0.05;

/// Where each previewed head is drawn, and the clocks of the last frame.
pub(super) struct Motors {
    drawn: HashMap<String, [f32; 2]>,
    /// The clock and the track time of the last frame, in seconds.
    last: Option<(f64, f32)>,
    /// What [`Self::follow`]'s clock counts from.
    epoch: Instant,
}

impl Default for Motors {
    fn default() -> Self {
        Self {
            drawn: HashMap::new(),
            last: None,
            epoch: Instant::now(),
        }
    }
}

impl Motors {
    /// Turn each head of `universe` from where it was drawn toward the pan
    /// and tilt the score sends, on the wall clock.
    pub(super) fn follow(
        &mut self,
        time: f32,
        universe: Option<UniverseState>,
    ) -> Option<UniverseState> {
        let clock = self.epoch.elapsed().as_secs_f64();
        self.step(clock, time, universe)
    }

    /// [`Self::follow`] on the caller's clock: `clock` is seconds on any
    /// clock that does not run backwards. A seek places every head on its
    /// target.
    pub(super) fn step(
        &mut self,
        clock: f64,
        time: f32,
        universe: Option<UniverseState>,
    ) -> Option<UniverseState> {
        let elapsed = self.last.map(|(at, then)| {
            let wall = (clock - at) as f32;
            let track = time - then;
            (wall, track < -SEEK_BACK_S || track > wall + SEEK_AHEAD_S)
        });
        self.last = Some((clock, time));
        let Some(mut universe) = universe else {
            self.drawn.clear();
            return None;
        };
        let step = match elapsed {
            Some((wall, false)) => wall * DEGREES_PER_SECOND,
            _ => {
                self.drawn.clear();
                0.0
            }
        };
        for (head, state) in &mut universe.primitives {
            let target = state.position;
            if target.iter().any(|v| !v.is_finite()) {
                continue;
            }
            let drawn = self.drawn.entry(head.clone()).or_insert(target);
            for (at, to) in drawn.iter_mut().zip(target) {
                *at += (to - *at).clamp(-step, step);
            }
            state.position = *drawn;
        }
        Some(universe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luma_lib::models::universe::PrimitiveState;

    fn at(position: [f32; 2]) -> Option<UniverseState> {
        Some(UniverseState {
            primitives: [(
                "fx:0".to_string(),
                PrimitiveState {
                    dimmer: 1.0,
                    color: [1.0; 3],
                    strobe: 0.0,
                    position,
                    speed: 1.0,
                    aim: None,
                },
            )]
            .into(),
        })
    }

    fn drawn(frame: &Option<UniverseState>) -> [f32; 2] {
        frame.as_ref().unwrap().primitives["fx:0"].position
    }

    #[test]
    fn a_head_turns_at_the_motor_speed_until_it_arrives() {
        let mut motors = Motors::default();
        let first = motors.step(0.0, 0.0, at([0.0, 0.0]));
        assert_eq!(drawn(&first), [0.0, 0.0]);
        // Half a second later the score wants 180° of pan: the head is drawn
        // about 90° along.
        let turning = motors.step(0.5, 0.5, at([180.0, 20.0]));
        let [pan, tilt] = drawn(&turning);
        assert!((85.0..=100.0).contains(&pan), "{pan}");
        assert_eq!(tilt, 20.0);
        // A second later it is there.
        let arrived = motors.step(1.5, 1.5, at([180.0, 20.0]));
        assert_eq!(drawn(&arrived), [180.0, 20.0]);
    }

    #[test]
    fn equal_clock_steps_turn_a_head_equally() {
        let mut motors = Motors::default();
        motors.step(0.0, 0.0, at([0.0, 0.0]));
        let first = drawn(&motors.step(1.0 / 60.0, 1.0 / 60.0, at([180.0, 0.0])))[0];
        let second = drawn(&motors.step(2.0 / 60.0, 2.0 / 60.0, at([180.0, 0.0])))[0];
        assert!((second - 2.0 * first).abs() < 1e-3, "{first} then {second}");
    }

    #[test]
    fn a_seek_places_every_head_on_its_target() {
        let mut motors = Motors::default();
        motors.follow(10.0, at([0.0, 0.0]));
        let sought = motors.follow(40.0, at([200.0, -90.0]));
        assert_eq!(drawn(&sought), [200.0, -90.0]);
        let back = motors.follow(5.0, at([0.0, 0.0]));
        assert_eq!(drawn(&back), [0.0, 0.0]);
    }
}
