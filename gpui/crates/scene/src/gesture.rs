//! Screen-space gesture arbitration shared by picking, orbit and marquee.

use crate::Camera;
use glam::{Vec2, Vec3};

/// A click may move this many logical pixels without becoming an orbit.
pub const DRAG_THRESHOLD: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenRect {
    pub min: Vec2,
    pub max: Vec2,
}

impl ScreenRect {
    #[must_use]
    pub fn from_points(a: Vec2, b: Vec2) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
        }
    }

    #[must_use]
    pub fn contains(self, point: Vec2) -> bool {
        point.cmpge(self.min).all() && point.cmple(self.max).all()
    }

    #[must_use]
    pub fn size(self) -> Vec2 {
        self.max - self.min
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClickOrbitUpdate {
    Pending,
    /// First orbit update; includes all movement since mouse-down.
    BeginOrbit(Vec2),
    /// Subsequent orbit update; includes movement since the previous update.
    Orbit(Vec2),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClickOrbitRelease {
    Click,
    Orbit,
}

/// Delays left-button orbit just long enough to distinguish a click.
#[derive(Clone, Copy, Debug)]
pub struct ClickOrbit {
    origin: Vec2,
    previous: Vec2,
    orbiting: bool,
}

impl ClickOrbit {
    #[must_use]
    pub fn new(origin: Vec2) -> Self {
        Self {
            origin,
            previous: origin,
            orbiting: false,
        }
    }

    pub fn moved(&mut self, position: Vec2) -> ClickOrbitUpdate {
        if self.orbiting {
            let delta = position - self.previous;
            self.previous = position;
            return ClickOrbitUpdate::Orbit(delta);
        }
        let total = position - self.origin;
        self.previous = position;
        if total.length_squared() > DRAG_THRESHOLD * DRAG_THRESHOLD {
            self.orbiting = true;
            ClickOrbitUpdate::BeginOrbit(total)
        } else {
            ClickOrbitUpdate::Pending
        }
    }

    #[must_use]
    pub fn released(self) -> ClickOrbitRelease {
        if self.orbiting {
            ClickOrbitRelease::Orbit
        } else {
            ClickOrbitRelease::Click
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Marquee {
    start: Vec2,
    current: Vec2,
}

impl Marquee {
    #[must_use]
    pub fn new(start: Vec2) -> Self {
        Self {
            start,
            current: start,
        }
    }

    pub fn moved(&mut self, current: Vec2) {
        self.current = current;
    }

    #[must_use]
    pub fn rect(self) -> ScreenRect {
        ScreenRect::from_points(self.start, self.current)
    }

    /// Legacy behavior: either dimension exceeding five pixels is a marquee.
    #[must_use]
    pub fn qualifies(self) -> bool {
        let size = self.rect().size();
        size.x > DRAG_THRESHOLD || size.y > DRAG_THRESHOLD
    }

    /// Project a Z-up world origin into logical viewport coordinates.
    #[must_use]
    pub fn contains_world(self, camera: &Camera, viewport: Vec2, world: Vec3) -> bool {
        let ndc = camera.project(world, viewport.x / viewport.y.max(1.0));
        let point = Vec2::new(
            (ndc.x * 0.5 + 0.5) * viewport.x,
            (-ndc.y * 0.5 + 0.5) * viewport.y,
        );
        self.rect().contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_pixels_remains_a_click_and_any_more_begins_orbit() {
        for endpoint in [Vec2::new(3.0, 4.0), Vec2::new(-3.0, -4.0)] {
            let mut gesture = ClickOrbit::new(Vec2::ZERO);
            assert_eq!(gesture.moved(endpoint), ClickOrbitUpdate::Pending);
            assert_eq!(gesture.released(), ClickOrbitRelease::Click);
        }
        let mut gesture = ClickOrbit::new(Vec2::new(10.0, 20.0));
        assert_eq!(
            gesture.moved(Vec2::new(15.01, 20.0)),
            ClickOrbitUpdate::BeginOrbit(Vec2::new(5.01, 0.0))
        );
        assert_eq!(gesture.released(), ClickOrbitRelease::Orbit);
    }

    #[test]
    fn orbit_receives_the_accumulated_then_incremental_delta() {
        let mut gesture = ClickOrbit::new(Vec2::splat(10.0));
        assert_eq!(
            gesture.moved(Vec2::new(16.0, 12.0)),
            ClickOrbitUpdate::BeginOrbit(Vec2::new(6.0, 2.0))
        );
        assert_eq!(
            gesture.moved(Vec2::new(18.0, 9.0)),
            ClickOrbitUpdate::Orbit(Vec2::new(2.0, -3.0))
        );
    }

    #[test]
    fn marquee_uses_axis_threshold_and_normalizes_direction() {
        for endpoint in [Vec2::new(5.0, 5.0), Vec2::new(-5.0, -5.0)] {
            let mut marquee = Marquee::new(Vec2::ZERO);
            marquee.moved(endpoint);
            assert!(!marquee.qualifies());
        }
        let mut marquee = Marquee::new(Vec2::new(10.0, 10.0));
        marquee.moved(Vec2::new(4.9, 9.0));
        assert!(marquee.qualifies());
        assert_eq!(marquee.rect().min, Vec2::new(4.9, 9.0));
        assert!(marquee.rect().contains(Vec2::new(7.0, 9.5)));
    }

    #[test]
    fn world_projection_uses_viewport_coordinates() {
        let camera = Camera::default();
        let viewport = Vec2::new(800.0, 400.0);
        let centre = camera.target;
        let mut around_centre = Marquee::new(Vec2::new(390.0, 190.0));
        around_centre.moved(Vec2::new(410.0, 210.0));
        assert!(around_centre.contains_world(&camera, viewport, centre));

        let projected = camera.project(Vec3::new(100.0, 100.0, 100.0), 2.0);
        let far_screen = Vec2::new(
            (projected.x * 0.5 + 0.5) * viewport.x,
            (-projected.y * 0.5 + 0.5) * viewport.y,
        );
        assert!(!around_centre.rect().contains(far_screen));
    }
}
