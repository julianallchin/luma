//! Laser output: each projector's scan, trimmed for its box and handed to
//! the DAC that plays it (`docs/design/lasers.md`, "Output").
//!
//! The output loop solves the rig's scans once a tick, the same solve the
//! visualizer draws, and publishes each projector's as the *latest* frame.
//! A driver plays the newest frame it has on repeat, at the scanner's own
//! rate, until the next replaces it.
//!
//! Nothing reaches a DAC until laser output is turned on, and it is off at
//! every launch: [`Shared::on`] is never stored.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::laser::{Key, LaserFrame, LaserRig, Settings};
use crate::models::lasers::{LaserDevice, LaserLink, LaserRoute};
use crate::models::universe::UniverseState;

/// One point as a DAC takes it: mirrors across the full signed range, each
/// diode across the full unsigned one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WirePoint {
    pub x: i16,
    pub y: i16,
    pub rgb: [u16; 3],
}

/// One projector's scan, ready for its DAC: played on repeat at `rate`
/// points a second.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WireFrame {
    pub rate: u32,
    pub points: Vec<WirePoint>,
    /// The longest move between two dark points, in DAC units: what a
    /// driver steps by when it must get from one frame to the next.
    pub blank_step: u16,
}

impl WireFrame {
    /// `frame` as `settings`' box needs it sent, at `master` of its light.
    ///
    /// The solver works in the scan's own terms: where the mirrors point and
    /// how hard each diode is driven. What differs from box to box is here:
    /// which way up it hangs and how its DAC is wired, how much of the DAC's
    /// range its scan takes and where its middle sits, how late its colour
    /// must be sent for the mirrors to have arrived, and the level at which
    /// each diode starts to give light.
    pub fn of(frame: &LaserFrame, settings: &Settings, master: f64) -> Self {
        let on = |key| settings.is_on(key);
        let place = |x: f32, y: f32| -> [i16; 2] {
            let x = f64::from(x) * if on(Key::FlipX) { -1. } else { 1. };
            let y = f64::from(y) * if on(Key::FlipY) { -1. } else { 1. };
            let x = x * settings.get(Key::SizeX) + settings.get(Key::OffsetX);
            let y = y * settings.get(Key::SizeY) + settings.get(Key::OffsetY);
            let [x, y] = if on(Key::Swap) { [y, x] } else { [x, y] };
            [x, y].map(|c| (c.clamp(-1., 1.) * f64::from(i16::MAX)).round() as i16)
        };
        let floor = [Key::FloorRed, Key::FloorGreen, Key::FloorBlue].map(|key| settings.get(key));
        let master = master.clamp(0., 1.);
        let light = |rgb: [f32; 3]| -> [u16; 3] {
            std::array::from_fn(|diode| {
                let level = f64::from(rgb[diode]).clamp(0., 1.) * master;
                if level <= 0. {
                    return 0;
                }
                let sent = floor[diode] + (1. - floor[diode]) * level;
                (sent * f64::from(u16::MAX)).round() as u16
            })
        };
        let n = frame.points.len();
        let delay = settings.get(Key::ColourDelay).round() as usize;
        let points = (0..n)
            .map(|i| {
                let [x, y] = place(frame.points[i].x, frame.points[i].y);
                // The scan repeats, so the colour that belongs `delay`
                // points back wraps round its end.
                let rgb = light(frame.points[(i + n - delay % n) % n].rgb);
                WirePoint { x, y, rgb }
            })
            .collect();
        let size = settings.get(Key::SizeX).max(settings.get(Key::SizeY));
        let blank_step = crate::laser::blank_step(settings) * size * f64::from(i16::MAX);
        WireFrame {
            rate: frame.rate,
            points,
            blank_step: blank_step.round().max(1.) as u16,
        }
    }
}

/// What every laser driver reads: the frames, the routes, and whether to
/// play; and what it writes: how each projector's DAC is doing.
pub(crate) struct Shared {
    /// The newest frame of each projector with something to draw.
    frames: Mutex<Arc<HashMap<String, Arc<WireFrame>>>>,
    routes: RwLock<Arc<Vec<LaserRoute>>>,
    /// Laser output. Off until someone turns it on, every launch.
    pub on: AtomicBool,
    pub shutdown: AtomicBool,
    links: Mutex<BTreeMap<String, LaserLink>>,
}

impl Shared {
    pub fn new(routes: Vec<LaserRoute>) -> Self {
        Self {
            frames: Mutex::default(),
            routes: RwLock::new(Arc::new(routes)),
            on: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            links: Mutex::default(),
        }
    }

    pub fn running(&self) -> bool {
        !self.shutdown.load(Ordering::Relaxed)
    }

    pub fn routes(&self) -> Arc<Vec<LaserRoute>> {
        self.routes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_routes(&self, routes: Vec<LaserRoute>) {
        *self.routes.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(routes);
    }

    /// The frame `projector` should be playing now: `None` while output is
    /// off or it has nothing to draw.
    pub fn frame(&self, projector: &str) -> Option<Arc<WireFrame>> {
        if !self.on.load(Ordering::Relaxed) {
            return None;
        }
        self.frames
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(projector)
            .cloned()
    }

    fn publish(&self, frames: HashMap<String, Arc<WireFrame>>) {
        *self.frames.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(frames);
    }

    /// Turn laser output on or off. Off forgets every frame: turned on
    /// again, a DAC waits for the next one.
    pub fn set_on(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
        if !on {
            self.publish(HashMap::new());
        }
    }

    /// Give `projector` a frame, as a tick of the output loop would.
    #[cfg(test)]
    pub fn show(&self, projector: &str, frame: Option<Arc<WireFrame>>) {
        let mut frames = self.frames.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = HashMap::clone(&frames);
        match frame {
            Some(frame) => next.insert(projector.to_owned(), frame),
            None => next.remove(projector),
        };
        *frames = Arc::new(next);
    }

    /// Say how `projector`'s DAC is doing; `None` when it is no longer
    /// bound.
    pub fn report(&self, projector: &str, link: Option<LaserLink>) {
        let mut links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        match link {
            Some(link) => links.insert(projector.to_owned(), link),
            None => links.remove(projector),
        };
    }

    pub fn links(&self) -> BTreeMap<String, LaserLink> {
        self.links.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// One kind of DAC. Built with the [`Shared`] state, it finds its devices,
/// connects to the ones a route names and plays them, on threads of its
/// own, and goes dark when it stops. Dropping it stops and joins them.
pub(crate) trait LaserDriver: Send + Sync {
    /// Every DAC this driver can see now, bound or not.
    fn devices(&self) -> Vec<LaserDevice>;
}

/// The rig the output loop solves, and the projector showing its test
/// pattern.
#[derive(Default)]
pub(crate) struct Solve {
    pub rig: LaserRig,
    pub testing: Option<String>,
}

impl Solve {
    /// Every projector's frame for `state`, at `master` of its light. The
    /// projector under test shows its test pattern and nothing else.
    pub fn frames(&self, state: &UniverseState, master: f64) -> HashMap<String, Arc<WireFrame>> {
        let solved = self.rig.solve_testing(state, self.testing.as_deref());
        solved
            .into_iter()
            .filter_map(|frame| {
                let settings = &self.rig.projector(&frame.projector)?.settings;
                let wire = WireFrame::of(&frame, settings, master);
                Some((frame.projector, Arc::new(wire)))
            })
            .collect()
    }
}

/// Publish this tick's frames for the drivers.
pub(crate) fn publish(shared: &Shared, solve: &Solve, state: &UniverseState, master: f64) {
    if !shared.on.load(Ordering::Relaxed) {
        return;
    }
    shared.publish(solve.frames(state, master));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::laser::tests::facing_house;
    use crate::laser::LaserPoint;

    fn frame(points: Vec<LaserPoint>) -> LaserFrame {
        LaserFrame {
            projector: "p".into(),
            rate: 30_000,
            points,
        }
    }

    fn point(x: f32, y: f32, level: f32) -> LaserPoint {
        LaserPoint {
            x,
            y,
            rgb: [level; 3],
        }
    }

    /// Untrimmed, a frame goes out as it was solved: the scan's edge is the
    /// DAC's, full drive is full scale, and dark is zero.
    #[test]
    fn an_untrimmed_frame_fills_the_dacs_range() {
        let wire = WireFrame::of(
            &frame(vec![point(-1., 1., 1.), point(0.5, 0., 0.)]),
            &Settings::default(),
            1.,
        );
        assert_eq!(
            wire.points[0],
            WirePoint {
                x: -i16::MAX,
                y: i16::MAX,
                rgb: [u16::MAX; 3]
            }
        );
        assert_eq!(wire.points[1].rgb, [0; 3]);
        assert!(wire.points[1].x > 0 && wire.rate == 30_000);
    }

    /// How the box hangs and how its DAC is wired: mirrored, swapped,
    /// shrunk and moved, in that order, and never past the DAC's range.
    #[test]
    fn a_frame_is_placed_for_its_box() {
        let at = |change: &[(Key, f64)]| {
            let mut settings = Settings::default();
            for (key, value) in change {
                settings.set(*key, *value);
            }
            let p = WireFrame::of(&frame(vec![point(0.5, 0.25, 1.)]), &settings, 1.).points[0];
            [f64::from(p.x), f64::from(p.y)]
                .map(|c| (c / f64::from(i16::MAX) * 100.).round() / 100.)
        };
        assert_eq!(at(&[]), [0.5, 0.25]);
        assert_eq!(at(&[(Key::FlipX, 1.)]), [-0.5, 0.25]);
        assert_eq!(at(&[(Key::FlipY, 1.)]), [0.5, -0.25]);
        assert_eq!(at(&[(Key::Swap, 1.)]), [0.25, 0.5]);
        assert_eq!(at(&[(Key::SizeX, 0.5), (Key::OffsetY, 0.5)]), [0.25, 0.75]);
        assert_eq!(at(&[(Key::OffsetX, 1.)]), [1., 0.25]);
    }

    /// A diode gives no light below its floor, so a lit point starts there;
    /// a dark one stays at zero, and the master scales the light, not the
    /// floor.
    #[test]
    fn light_starts_at_each_diodes_floor() {
        let mut settings = Settings::default();
        settings.set(Key::FloorGreen, 0.2);
        let sent = |level: f32, master: f64| {
            let wire = WireFrame::of(&frame(vec![point(0., 0., level)]), &settings, master);
            wire.points[0]
                .rgb
                .map(|c| f64::from(c) / f64::from(u16::MAX))
        };
        let near = |a: f64, b: f64| (a - b).abs() < 1e-4;
        assert_eq!(sent(0., 1.), [0.; 3]);
        assert!(near(sent(1., 1.)[1], 1.) && near(sent(0.5, 1.)[1], 0.6));
        assert!(near(sent(0.5, 1.)[0], 0.5));
        assert!(near(sent(1., 0.5)[1], 0.6));
        assert_eq!(sent(1., 0.), [0.; 3]);
    }

    /// The colour is sent later than the place it belongs to, round the
    /// scan's end.
    #[test]
    fn colour_is_sent_late() {
        let mut settings = Settings::default();
        settings.set(Key::ColourDelay, 1.);
        let wire = WireFrame::of(
            &frame(vec![
                point(0., 0., 1.),
                point(0.1, 0., 0.),
                point(0.2, 0., 0.),
            ]),
            &settings,
            1.,
        );
        let lit: Vec<bool> = wire.points.iter().map(|p| p.rgb[0] > 0).collect();
        assert_eq!(lit, [false, true, false]);
    }

    /// Nothing is published while output is off, whatever plays.
    #[test]
    fn nothing_is_published_while_output_is_off() {
        let shared = Shared::new(Vec::new());
        let solve = Solve {
            rig: LaserRig {
                projectors: vec![facing_house("p", 0.)],
                window: None,
                shared: Settings::default(),
            },
            testing: Some("p".into()),
        };
        publish(&shared, &solve, &UniverseState::default(), 1.);
        assert!(shared.frame("p").is_none());
        shared.set_on(true);
        publish(&shared, &solve, &UniverseState::default(), 1.);
        assert!(shared.frame("p").is_some());
        // Turned off, a driver is given nothing; turned on again, nothing
        // until the next frame.
        shared.set_on(false);
        assert!(shared.frame("p").is_none());
        shared.set_on(true);
        assert!(shared.frame("p").is_none());
    }

    /// The test pattern is the window's edge and its middle, dimmed, and
    /// replaces what the projector under test would play.
    #[test]
    fn the_test_pattern_draws_the_windows_edge() {
        let solve = Solve {
            rig: LaserRig {
                projectors: vec![facing_house("p", 0.), facing_house("q", 2.)],
                window: None,
                shared: Settings::default(),
            },
            testing: Some("p".into()),
        };
        let frames = solve.frames(&UniverseState::default(), 1.);
        assert!(frames.contains_key("p") && !frames.contains_key("q"));
        let lit: Vec<&WirePoint> = frames["p"]
            .points
            .iter()
            .filter(|p| p.rgb != [0; 3])
            .collect();
        let peak = lit.iter().flat_map(|p| p.rgb).max().unwrap();
        assert!(f64::from(peak) <= crate::laser::TEST_LEVEL * f64::from(u16::MAX) + 1.);
        // It reaches all four sides of the window, and no further: inside
        // the scan.
        let (xs, ys): (Vec<i16>, Vec<i16>) = lit.iter().map(|p| (p.x, p.y)).unzip();
        let span = |c: &[i16]| (*c.iter().min().unwrap(), *c.iter().max().unwrap());
        let ((left, right), (bottom, top)) = (span(&xs), span(&ys));
        assert!(left < 0 && right > 0 && top > bottom);
        assert!(right < i16::MAX && left > -i16::MAX && top < i16::MAX);
    }
}
