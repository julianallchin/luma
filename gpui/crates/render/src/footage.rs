//! The footage look's camera, and the clock every frame is drawn on.
//!
//! Live and export share this. Each gives the clock at the end of a frame and
//! how long since the last one; [`moment`] answers with the moment to build
//! and render. A frame is one moment: its strobes show the share of the
//! interval their gate is on (`strobe.rs`). With the footage look on, a hand
//! on the camera and the bass shake it ([`shake`]); the sensor noise lives in
//! the post chain.

use glam::{Mat3, Vec3};

use crate::frame::{Camera, Moment};
use crate::scene_desc::Footage;
use crate::strobe::Slice;

/// The frame interval a caller without a display assumes: 60 frames a second.
pub const FRAME_S: f64 = 1.0 / 60.0;

/// The frame that ends at `end` on the free-running clock, `interval` seconds
/// after the frame before it. `bass` is the playing track's low-band envelope
/// at `end`.
#[must_use]
pub fn moment(end: f64, interval: f64, bass: f32) -> Moment {
    let interval = if interval.is_finite() && interval > 0.0 {
        interval
    } else {
        FRAME_S
    };
    Moment {
        time: end,
        slice: Slice {
            open: end - interval,
            length: interval,
        },
        bass,
    }
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
    fn a_frame_is_one_moment_whose_slice_is_the_whole_interval() {
        let moment = moment(10.0, 0.02, 0.5);
        assert_eq!(moment.time, 10.0);
        assert_eq!(moment.bass, 0.5);
        assert!((moment.slice.open - 9.98).abs() < 1e-12);
        assert!((moment.slice.length - 0.02).abs() < 1e-12);
        let fallback = super::moment(10.0, f64::NAN, 0.0);
        assert_eq!(fallback.slice.length, FRAME_S);
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
