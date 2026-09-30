//! The footage look's clock and camera: which moments one frame's shutter
//! holds, and where a hand on the camera points it at each.
//!
//! Live and export share this. Each gives the clock at the end of a frame and
//! how long since the last one; [`moments`] answers with the moments to build
//! and render. The only difference between the two is how many subframes
//! they can afford.
//!
//! With the footage look off a frame is one moment, but its strobes still
//! integrate over the whole interval since the previous frame, so no flash
//! falls between two frames. With it on, the shutter is open for its angle's
//! share of the interval and each subframe is a moment of its own, drawn
//! whole: heads, beams, strobes and camera move within the frame and blur.

use glam::{Mat3, Vec3};

use crate::frame::{Camera, Moment};
use crate::scene_desc::Footage;
use crate::strobe::Slice;

/// The frame interval a caller without a display assumes: 60 frames a second.
pub const FRAME_S: f64 = 1.0 / 60.0;

/// The moments of one frame that ends at `end` on the free-running clock,
/// `interval` seconds after the frame before it. `subframes` is how many the
/// renderer can afford when the footage look is on. `bass` is the playing
/// track's low-band envelope at a clock time.
///
/// In time order. The shutter closes at `end`, so the last moment is the
/// newest: a frame shows the exposure that has just finished, as a camera
/// does.
#[must_use]
pub fn moments(
    footage: &Footage,
    end: f64,
    interval: f64,
    subframes: u32,
    bass: impl Fn(f64) -> f32,
) -> Vec<Moment> {
    let interval = if interval.is_finite() && interval > 0.0 {
        interval
    } else {
        FRAME_S
    };
    if !footage.enabled {
        return vec![Moment {
            time: end,
            slice: Slice {
                open: end - interval,
                length: interval,
                readout: 0.0,
            },
            bass: bass(end),
        }];
    }
    let footage = footage.sanitized();
    let open = interval * f64::from(footage.shutter_deg) / 360.0;
    let count = subframes.max(1);
    let length = open / f64::from(count);
    (0..count)
        .map(|k| {
            let start = end - open + f64::from(k) * length;
            let time = start + length / 2.0;
            Moment {
                time,
                slice: Slice {
                    open: start,
                    length,
                    readout: f64::from(footage.readout_ms) / 1000.0,
                },
                bass: bass(time),
            }
        })
        .collect()
}

/// Handheld sway at `handheld = 1`, in degrees: a hand on a camera wanders
/// about this far, slowly.
const HANDHELD_DEG: f32 = 0.5;
/// Bass shake at `bass = 1` and a full envelope, in degrees.
const BASS_DEG: f32 = 0.4;

/// A sum of sines with incommensurate frequencies: smooth, never repeating
/// in a show's length, and the same at the same time on every run. Each
/// entry is `(hz, phase, weight)`; the weights sum to one, so the sum stays
/// within ±1.
fn sway(time: f64, waves: &[(f64, f64, f32)]) -> f32 {
    waves
        .iter()
        .map(|&(hz, phase, weight)| {
            weight * (std::f64::consts::TAU * (hz * time + phase)).sin() as f32
        })
        .sum()
}

/// Yaw and pitch, in radians, of the camera shake at `moment`.
#[must_use]
pub fn shake(footage: &Footage, moment: &Moment) -> [f32; 2] {
    if !footage.enabled {
        return [0.0, 0.0];
    }
    let footage = footage.sanitized();
    let time = moment.time;
    // Between 0.5 and 2 Hz: breathing and a shifting stance.
    let hand = (footage.handheld * HANDHELD_DEG).to_radians();
    let hand = [
        sway(
            time,
            &[(0.53, 0.11, 0.6), (1.17, 0.47, 0.3), (1.91, 0.83, 0.1)],
        ),
        sway(
            time,
            &[(0.61, 0.29, 0.6), (1.33, 0.71, 0.3), (1.79, 0.05, 0.1)],
        ),
    ]
    .map(|v| v * hand);
    // Fast, and only as strong as the bass is loud: a floor or a camera
    // body shaken by the subs.
    let kick = (footage.bass * BASS_DEG).to_radians() * moment.bass.clamp(0.0, 1.0);
    let kick = [
        sway(
            time,
            &[(7.3, 0.37, 0.5), (11.9, 0.13, 0.3), (17.1, 0.59, 0.2)],
        ),
        sway(
            time,
            &[(8.1, 0.91, 0.5), (12.7, 0.23, 0.3), (15.3, 0.67, 0.2)],
        ),
    ]
    .map(|v| v * kick);
    [hand[0] + kick[0], hand[1] + kick[1]]
}

/// `camera` turned by `[yaw, pitch]` radians about its own eye. The eye stays
/// where it is: a hand turns a camera, it does not carry it around the room.
#[must_use]
pub fn turned(camera: Camera, [yaw, pitch]: [f32; 2]) -> Camera {
    if yaw == 0.0 && pitch == 0.0 {
        return camera;
    }
    let forward = camera.target - camera.eye;
    let distance = forward.length();
    let Some(direction) = forward.try_normalize() else {
        return camera;
    };
    // World up is +Z, as `gpu::camera_matrices` looks at it.
    let right = direction.cross(Vec3::Z).try_normalize().unwrap_or(Vec3::X);
    let turned =
        Mat3::from_axis_angle(Vec3::Z, yaw) * Mat3::from_axis_angle(right, pitch) * direction;
    Camera {
        target: camera.eye + turned * distance,
        ..camera
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Footage {
        Footage {
            enabled: true,
            ..Footage::OFF
        }
    }

    #[test]
    fn off_is_one_moment_whose_strobes_cover_the_whole_interval() {
        let moments = moments(&Footage::OFF, 10.0, 0.02, 16, |_| 0.0);
        assert_eq!(moments.len(), 1);
        assert_eq!(moments[0].time, 10.0);
        assert!((moments[0].slice.open - 9.98).abs() < 1e-12);
        assert!((moments[0].slice.length - 0.02).abs() < 1e-12);
    }

    #[test]
    fn the_shutter_slices_tile_the_open_part_of_the_interval() {
        let footage = on();
        for count in [1, 2, 16] {
            let moments = moments(&footage, 10.0, 0.02, count, |_| 0.0);
            assert_eq!(moments.len(), count as usize);
            // 180 degrees of a 20 ms frame, closing at the frame's end.
            assert!((moments[0].slice.open - 9.99).abs() < 1e-12);
            for pair in moments.windows(2) {
                let (a, b) = (pair[0].slice, pair[1].slice);
                assert!((a.open + a.length - b.open).abs() < 1e-12);
                assert!(pair[0].time < pair[1].time);
            }
            let last = moments.last().unwrap().slice;
            assert!((last.open + last.length - 10.0).abs() < 1e-12);
            assert!(moments
                .iter()
                .all(|m| (m.slice.readout - 0.01).abs() < 1e-12));
        }
    }

    #[test]
    fn a_hand_turns_the_camera_a_little_and_never_moves_it() {
        let camera = Camera {
            eye: Vec3::new(3.0, -8.0, 2.0),
            target: Vec3::new(0.0, 0.0, 1.5),
            fov_y_deg: 50.0,
        };
        let footage = Footage {
            handheld: 1.0,
            ..on()
        };
        let mut largest = 0.0_f32;
        let mut previous: Option<Camera> = None;
        for step in 0..600 {
            let moment = Moment::at(f64::from(step) / 60.0);
            let shaken = turned(camera, shake(&footage, &moment));
            assert_eq!(shaken.eye, camera.eye);
            let angle = (camera.target - camera.eye)
                .angle_between(shaken.target - shaken.eye)
                .to_degrees();
            largest = largest.max(angle);
            // Continuous: a sixtieth of a second moves it a small fraction.
            if let Some(previous) = previous {
                let step = (previous.target - previous.eye)
                    .angle_between(shaken.target - shaken.eye)
                    .to_degrees();
                assert!(step < 0.1, "{step}");
            }
            previous = Some(shaken);
        }
        assert!((0.1..=0.75).contains(&largest), "{largest}");
    }

    #[test]
    fn bass_shake_follows_the_envelope() {
        let footage = Footage {
            handheld: 0.0,
            bass: 1.0,
            ..on()
        };
        let quiet = Moment {
            bass: 0.0,
            ..Moment::at(1.234)
        };
        assert_eq!(shake(&footage, &quiet), [0.0, 0.0]);
        let loud = (0..60)
            .map(|step| {
                let moment = Moment {
                    bass: 1.0,
                    ..Moment::at(1.0 + f64::from(step) / 240.0)
                };
                shake(&footage, &moment)[0].abs()
            })
            .fold(0.0_f32, f32::max);
        assert!(loud > 0.05_f32.to_radians(), "{loud}");
        assert!(loud <= BASS_DEG.to_radians() + 1e-6);
    }
}
