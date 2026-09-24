//! Preview motor lag: the stage shows what a real head can do.
//!
//! The score and the DMX output are never slowed. Only the drawn beam turns
//! toward the solver's pan and tilt at a motor's speed, and while it is more
//! than [`SHOW_TARGET_DEG`] off, the stage draws a faint line where the score
//! wants it. See `docs/specs/aim.md`, "Preview motor lag".
use std::collections::HashMap;
use std::time::Instant;

use luma_lib::models::universe::UniverseState;
use luma_render::scene_desc::AimTarget;

/// How fast a previewed head turns on each axis. Fixture profiles carry no
/// motor speed, so every head gets this one.
const DEGREES_PER_SECOND: f32 = 180.0;

/// A drawn beam further than this from its target shows the target line.
const SHOW_TARGET_DEG: f32 = 3.0;

/// Track time that runs ahead of the wall clock by more than this, or back
/// by more than [`SEEK_BACK_S`], is a seek.
const SEEK_AHEAD_S: f32 = 0.25;
const SEEK_BACK_S: f32 = 0.05;

/// Where each previewed head is drawn, and the clocks of the last frame.
#[derive(Default)]
pub(super) struct Motors {
    drawn: HashMap<String, [f32; 2]>,
    last: Option<(Instant, f32)>,
}

impl Motors {
    /// Turn each head of `universe` from where it was drawn toward the pan
    /// and tilt the score sends, and say which heads are still off target.
    /// A seek places every head on its target.
    pub(super) fn follow(
        &mut self,
        time: f32,
        universe: Option<UniverseState>,
    ) -> (Option<UniverseState>, Vec<AimTarget>) {
        let now = Instant::now();
        let elapsed = self.last.map(|(at, then)| {
            let wall = (now - at).as_secs_f32();
            let track = time - then;
            (wall, track < -SEEK_BACK_S || track > wall + SEEK_AHEAD_S)
        });
        self.last = Some((now, time));
        let Some(mut universe) = universe else {
            self.drawn.clear();
            return (None, Vec::new());
        };
        let step = match elapsed {
            Some((wall, false)) => wall * DEGREES_PER_SECOND,
            _ => {
                self.drawn.clear();
                0.0
            }
        };
        let mut targets = Vec::new();
        for (head, state) in &mut universe.primitives {
            let target = state.position;
            if target.iter().any(|v| !v.is_finite()) {
                continue;
            }
            let drawn = self.drawn.entry(head.clone()).or_insert(target);
            for (at, to) in drawn.iter_mut().zip(target) {
                *at += (to - *at).clamp(-step, step);
            }
            if luma_render::luminaire::degrees_between(*drawn, target) > SHOW_TARGET_DEG {
                targets.push(AimTarget {
                    head: head.clone(),
                    position: target,
                });
            }
            state.position = *drawn;
        }
        (Some(universe), targets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luma_lib::models::universe::PrimitiveState;
    use std::time::Duration;

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

    fn drawn(frame: &(Option<UniverseState>, Vec<AimTarget>)) -> [f32; 2] {
        frame.0.as_ref().unwrap().primitives["fx:0"].position
    }

    #[test]
    fn a_head_turns_at_the_motor_speed_and_shows_its_target_until_it_arrives() {
        let mut motors = Motors::default();
        let first = motors.follow(0.0, at([0.0, 0.0]));
        assert_eq!(drawn(&first), [0.0, 0.0]);
        assert!(first.1.is_empty());
        // Half a second later the score wants 180° of pan: the head is drawn
        // about 90° along, with the target line showing.
        let start = Instant::now() - Duration::from_millis(500);
        motors.last = Some((start, 0.0));
        let turning = motors.follow(0.5, at([180.0, 20.0]));
        let [pan, tilt] = drawn(&turning);
        assert!((85.0..=100.0).contains(&pan), "{pan}");
        assert_eq!(tilt, 20.0);
        assert_eq!(turning.1.len(), 1);
        assert_eq!(turning.1[0].position, [180.0, 20.0]);
        // A second later it is there, and the line is gone.
        motors.last = Some((Instant::now() - Duration::from_secs(1), 0.5));
        let arrived = motors.follow(1.5, at([180.0, 20.0]));
        assert_eq!(drawn(&arrived), [180.0, 20.0]);
        assert!(arrived.1.is_empty());
    }

    #[test]
    fn a_seek_places_every_head_on_its_target() {
        let mut motors = Motors::default();
        motors.follow(10.0, at([0.0, 0.0]));
        let sought = motors.follow(40.0, at([200.0, -90.0]));
        assert_eq!(drawn(&sought), [200.0, -90.0]);
        assert!(sought.1.is_empty());
        let back = motors.follow(5.0, at([0.0, 0.0]));
        assert_eq!(drawn(&back), [0.0, 0.0]);
    }
}
