//! Moving the eye: orbit, pan, fly and zoom, the pivot dot, and the frame
//! every camera in the viewport is fitted to.
use super::*;

/// Wheel zoom: one wheel event's factor is
/// `ZOOM_BASE ** (ZOOM_SPEED · distance · 0.05)`, and the eye moves that share
/// of the distance to the pivot — see [`Camera::zoom_toward`].
const ZOOM_SPEED: f32 = 0.5;
const ZOOM_BASE: f32 = 0.95;

/// The zoom factors of the toolbar's zoom-in and zoom-out buttons and of the
/// `=` and `-` keys.
pub(crate) const ZOOM_IN: f32 = 0.8;
pub(crate) const ZOOM_OUT: f32 = 1.25;

/// How long the pivot dot shows after an orbit starts or a wheel step.
const PIVOT_DOT: Duration = Duration::from_millis(600);
/// Diameter of the pivot dot.
pub(super) const PIVOT_DOT_SIZE: Pixels = px(7.);

/// A fly's look turns half a turn per pane height: half the orbit's rate,
/// because the whole room swings past a look and only the rig past an orbit.
const LOOK_RATE: f32 = std::f32::consts::PI;
/// The range the wheel may set the fly speed multiple in.
const FLY_SPEED_MULTIPLE: (f32, f32) = (0.05, 20.0);

/// What the pointer is doing to the camera.
///
/// Named states rather than a button plus a flag: the three are exclusive and
/// each consumes the pointer delta differently, so a call site holding "this
/// button, but panning" could say something the camera has no answer for.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Drag {
    /// Left button: turn the eye and the target about the point that was
    /// under the pointer when the drag started.
    Orbit { pivot: Vec3 },
    /// Middle button: slide the eye and the target across the view plane, at
    /// the depth of what was under the pointer when the drag started.
    Pan { depth: f32 },
    /// Right button: turn in place, and fly on the keys — see [`Fly`].
    Fly,
}

/// A fly in progress: the right button is held.
///
/// W and S move along the view, A and D sideways, Q and E down and up, Shift
/// faster, and the wheel sets the speed. The stage's own letters are off
/// while it lasts — see the key context in [`visualizer`].
pub(super) struct Fly {
    keys: FlyKeys,
    /// The previous frame's step, which the next one measures its time from.
    last: Instant,
    /// Whether the camera moved or turned. A right click that did neither
    /// leaves the camera exactly as it was.
    moved: bool,
    /// Whether the pointer lock was asked for yet, and whether it took. A
    /// locked pointer is hidden and held where it was pressed, and the look
    /// reads its relative motion once per frame; without a lock the look
    /// follows the pointer's moves, which a visible cursor then shows.
    lock_asked: bool,
    locked: bool,
}

impl Drag {
    /// The drag a button other than the left starts: the middle pans and the
    /// right flies, as in Unity and Unreal. The left orbits, through
    /// [`EditorDrag::ClickOrbit`], because a left click also selects.
    pub(super) fn of(button: MouseButton) -> Option<Self> {
        match button {
            MouseButton::Middle => Some(Self::Pan { depth: 0.0 }),
            MouseButton::Right => Some(Self::Fly),
            _ => None,
        }
    }
}

impl Visualizer {
    /// The frame every camera in this viewport is fitted to: its shape, and the
    /// band of it the floating chrome covers.
    ///
    /// Only chrome that *spans* an edge earns an inset, because that is what an
    /// inset claims — this band of the frame is covered. The toolbar's row runs
    /// the full width and is centred on exactly where a fitted rig's floor
    /// lands, so it does when it contains controls. An empty toolbar earns no
    /// inset. The frame-stats readout is a box in the top-left
    /// corner and does not: reserving the whole top band for it cost 19% of the
    /// pane's height in the 943×220 viewport the pixel suite opens, which with
    /// the toolbar's own band left the fit 55% of the frame to work in and drew
    /// the rig at half size. Chrome that floats in a corner frames as
    /// background.
    ///
    /// Before the first layout there is no shape to read, so a landscape
    /// default stands in and [`Visualizer::owes_opening_pose`] re-fits once
    /// there is one.
    pub(super) fn view_finder(&self) -> Viewfinder {
        let (w, h) = (f32::from(self.size.width), f32::from(self.size.height));
        if w <= 0.0 || h <= 0.0 {
            return Viewfinder::new(FOV_Y_DEG, DEFAULT_ASPECT);
        }
        let toolbar = if self.bottom_bar_visible {
            (f32::from(TOOLBAR_OVERLAY_BOTTOM) + f32::from(OVERLAY_BAND)) / h
        } else {
            0.
        };
        Viewfinder::new(FOV_Y_DEG, w / h).inset(Insets::vertical(0.0, toolbar))
    }

    /// Fit the selected objects' combined geometry bounds into the usable viewport.
    pub(crate) fn focus_selection(&mut self) -> bool {
        let mut bounds = Aabb::EMPTY;
        {
            let stage = self.stage.borrow();
            for object in self.selection.selected() {
                let (EditorObject::Fixture(id) | EditorObject::StagePiece(id)) = object;
                if let Some(object_bounds) = stage
                    .displayed_pick
                    .as_ref()
                    .and_then(|pick| pick.geometry.bounds.get(object).copied())
                    .or_else(|| self.build.as_ref().and_then(|build| build.room_bounds(id)))
                {
                    bounds.union(&object_bounds);
                }
            }
        }
        if bounds.is_empty() {
            return false;
        }
        let framing = Framing::of([], [bounds]);
        let direction = self.camera.position() - self.camera.target;
        self.camera = framing.fit(bounds.center(), direction, &self.view_finder());
        true
    }

    /// Zoom by a factor at a window point, or at the pane's centre — the
    /// wheel, the toolbar buttons and the `=`/`-` keys. The step aims at the
    /// point [`Self::surface_under`] finds there; see [`Camera::zoom_toward`].
    pub(crate) fn zoom(&mut self, factor: f32, at: Option<Point<Pixels>>) {
        let viewport = self.viewport_size();
        let at = at.map_or(viewport / 2.0, |point| self.viewport_point(point));
        let hit = self.surface_under(at).map(|hit| hit.point);
        let ray = self.pointer_ray(at);
        let limits = self.zoom_limits();
        let pivot = self.camera.zoom_toward(factor, ray, hit, &limits);
        self.pivot_dot = Some((pivot, Instant::now()));
    }

    fn zoom_limits(&self) -> ZoomLimits {
        self.framing
            .zoom_limits(opening_camera(&self.framing, &self.view_finder()).radius)
    }

    /// The ray through a viewport point, off the live camera. Before the pane
    /// has a size — the headless path — every point is the centre's ray.
    fn pointer_ray(&self, at: Vec2) -> luma_scene::Ray {
        let viewport = self.viewport_size();
        if viewport.min_element() <= 1.0 {
            return self.camera.ray(Vec2::ZERO, DEFAULT_ASPECT);
        }
        self.camera.pixel_ray(at, viewport)
    }

    /// What is under a viewport point, from a small area around it: see
    /// [`navigate::area_pick`]. Cast against the displayed frame's pick
    /// geometry, which is world space, through the *live* camera, which may
    /// have moved since that frame. Fixtures and pieces are the rig; a deck
    /// and the ground plane the renderer draws at `z = 0` are floor.
    fn surface_under(&self, at: Vec2) -> Option<SurfaceHit> {
        if self.viewport_size().min_element() <= 1.0 {
            return None;
        }
        let stage = self.stage.borrow();
        let pick = stage.displayed_pick.as_ref();
        let is_floor = |id: &str| {
            stage.scene.as_ref().is_some_and(|scene| {
                scene
                    .pieces
                    .iter()
                    .any(|piece| piece.id == id && piece.is_floor())
            })
        };
        let rays = navigate::pick_offsets().map(|offset| self.pointer_ray(at + offset));
        navigate::area_pick(rays, |ray| {
            let mesh = pick.and_then(|pick| {
                let hit = pick
                    .geometry
                    .graph
                    .raycast(ray, Default::default(), pick)
                    .into_iter()
                    .next()?;
                let surface = match pick.geometry.objects.get(hit.node.0 as usize)? {
                    Some(EditorObject::StagePiece(id)) if is_floor(id) => Surface::Floor,
                    _ => Surface::Rig,
                };
                Some(SurfaceHit {
                    point: hit.point,
                    distance: hit.t,
                    surface,
                })
            });
            [mesh, navigate::ground_hit(ray, 0.0)]
                .into_iter()
                .flatten()
                .min_by(|a, b| a.distance.total_cmp(&b.distance))
        })
    }

    /// The pivot for an orbit starting at a viewport point: what is under it,
    /// or the point the current distance reaches along its ray.
    pub(super) fn pivot_at(&mut self, at: Vec2) -> Vec3 {
        let pivot = self.surface_under(at).map_or_else(
            || self.camera.position() + self.pointer_ray(at).dir * self.camera.radius,
            |hit| hit.point,
        );
        self.pivot_dot = Some((pivot, Instant::now()));
        pivot
    }

    /// How deep a pan starting at a viewport point grabs: the view-axis
    /// distance to what is under it, or the target's when nothing is.
    fn pan_depth(&mut self, at: Vec2) -> f32 {
        let Some(hit) = self.surface_under(at) else {
            return self.camera.radius;
        };
        self.pivot_dot = Some((hit.point, Instant::now()));
        let eye = self.camera.position();
        let forward = (self.camera.target - eye).normalize();
        (hit.point - eye).dot(forward).max(self.zoom_limits().near)
    }

    /// The pivot dot's viewport position and opacity, while it shows.
    pub(super) fn pivot_dot_at(&self, now: Instant) -> Option<(Vec2, f32)> {
        let (pivot, since) = self.pivot_dot?;
        let age = now.saturating_duration_since(since).as_secs_f32() / PIVOT_DOT.as_secs_f32();
        let viewport = self.viewport_size();
        if age >= 1.0 || viewport.min_element() <= 1.0 {
            return None;
        }
        let ndc = self.camera.project(pivot, viewport.x / viewport.y);
        if !(0.0..=1.0).contains(&ndc.z) {
            return None;
        }
        let at = Vec2::new(
            (ndc.x + 1.0) * 0.5 * viewport.x,
            (1.0 - ndc.y) * 0.5 * viewport.y,
        );
        // Holds, then fades out.
        Some((at, 1.0 - age * age))
    }

    /// Start the drag a non-left button makes.
    pub(super) fn begin_drag(&mut self, drag: Drag, at: Point<Pixels>) {
        let drag = match drag {
            Drag::Pan { .. } => Drag::Pan {
                depth: self.pan_depth(self.viewport_point(at)),
            },
            drag => drag,
        };
        if drag == Drag::Fly {
            self.fly = Some(Fly {
                keys: FlyKeys::default(),
                last: Instant::now(),
                moved: false,
                lock_asked: false,
                locked: false,
            });
        }
        self.drag = Some((drag, at));
    }

    /// End whatever drag is live. A fly that moved hands back an orbit whose
    /// target is what the view is centred on, or the old distance ahead.
    pub(super) fn end_drag(&mut self) {
        self.drag = None;
        let Some(fly) = self.fly.take() else { return };
        if !fly.moved {
            return;
        }
        let eye = self.camera.position();
        let limits = self.zoom_limits();
        let distance = self
            .surface_under(self.viewport_size() / 2.0)
            .map_or(self.camera.radius, |hit| hit.point.distance(eye));
        self.camera.refocus(distance.clamp(limits.near, limits.far));
    }

    /// Whether pointer and key input must invalidate the window to be seen.
    ///
    /// Not during a fly. The fly redraws every display frame anyway (the
    /// stage asks for the next frame on every render), and each frame reads
    /// the look and the keys. An invalidation between frames is worse than
    /// useless: gpui draws a dirty window *synchronously* before it
    /// dispatches a key event, so with a mouse look dirtying the window at
    /// pointer rate, every key repeat of a held A or W ran an extra draw —
    /// a whole stage frame built and submitted, and the fly stepped, at the
    /// key event's moment rather than the display's. That was the stutter of
    /// looking while strafing.
    pub(super) fn input_redraws(&self) -> bool {
        self.fly.is_none()
    }

    /// A key while the right button is held. Whether the fly took it.
    pub(crate) fn fly_key(&mut self, key: &str, down: bool) -> bool {
        self.fly
            .as_mut()
            .is_some_and(|fly| fly.keys.set(&key.to_ascii_lowercase(), down))
    }

    /// One frame of the fly, if one is live.
    pub(super) fn fly_tick(&mut self, now: Instant, fast: bool) {
        let Some(fly) = self.fly.as_mut() else { return };
        // Capped, so a frame that stalled does not throw the eye across the room.
        let dt = now
            .saturating_duration_since(fly.last)
            .as_secs_f32()
            .min(0.1);
        fly.last = now;
        let keys = FlyKeys { fast, ..fly.keys };
        if !keys.moving() {
            return;
        }
        fly.moved = true;
        let speed = navigate::fly_speed(&self.framing) * self.fly_speed;
        self.camera.fly(keys, speed, dt, self.framing.floor_z());
    }

    /// A wheel event: a zoom at the pointer, or during a fly its speed. A
    /// wheel with no vertical travel — a sideways trackpad swipe, a gesture's
    /// end — is not a zoom: even a unit step re-aims the target at the
    /// pointer's depth and flashes the pivot.
    pub(super) fn wheel(&mut self, wheel: f32, at: Point<Pixels>) {
        if wheel == 0.0 {
            return;
        }
        let factor = zoom_scale(-wheel);
        if self.fly.is_some() {
            let (lo, hi) = FLY_SPEED_MULTIPLE;
            self.fly_speed = (self.fly_speed / factor).clamp(lo, hi);
        } else {
            self.zoom(factor, Some(at));
        }
    }

    /// Turn the camera in place by a pointer motion, in logical pixels: the
    /// orbit's turn about the eye.
    fn look(&mut self, dx: f32, dy: f32) {
        let turn = LOOK_RATE / f32::from(self.size.height).max(1.0);
        let eye = self.camera.position();
        self.camera
            .orbit_about(eye, -turn * dx, -turn * dy, &self.framing);
        if let Some(fly) = self.fly.as_mut() {
            fly.moved = true;
        }
    }

    /// Hold the platform's pointer lock to the fly: take it when a fly starts,
    /// give it back when none is live, and while it is held turn the look by
    /// the relative motion since the last call. Called from the press and
    /// release listeners, which is where the lock should begin and end, and
    /// once per frame, which is where the look is read and where a fly that
    /// ended some other way lets go.
    ///
    /// A locked pointer sends no moves at all, so every one of the mouse's
    /// thousand-a-second counts lands here as one summed turn per frame,
    /// and the compositor never has to draw a moving cursor.
    pub(super) fn sync_pointer_lock(&mut self, window: &mut Window) {
        match self.fly.as_mut() {
            Some(fly) if !fly.lock_asked => {
                fly.lock_asked = true;
                fly.locked = window.set_pointer_lock(true);
                self.pointer_locked = fly.locked;
            }
            None if self.pointer_locked => {
                window.set_pointer_lock(false);
                self.pointer_locked = false;
            }
            _ => {}
        }
        if self.fly.as_ref().is_some_and(|fly| fly.locked) {
            let delta = window.take_pointer_delta();
            if delta != Point::default() {
                self.look(f32::from(delta.x), f32::from(delta.y));
            }
        }
    }

    /// Consume one pointer step, in logical pixels.
    pub(super) fn dragged(&mut self, delta: Point<Pixels>) {
        let Some((drag, _)) = self.drag else { return };
        // Every one of three's rates is per element height, so a drag across a
        // tall panel turns the camera as far as the same drag across a short one.
        let height = f32::from(self.size.height).max(1.0);
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        match drag {
            // `rotateLeft(2π·dx/H)` and `rotateUp(2π·dy/H)`, both of which
            // *subtract* from the spherical angle, turned about the picked
            // pivot rather than the target. The polar keeps the orbit clamp,
            // so the eye stays above the floor and can still look up.
            Drag::Orbit { pivot } => {
                let turn = std::f32::consts::TAU / height;
                self.camera
                    .orbit_about(pivot, -turn * dx, -turn * dy, &self.framing);
            }
            // The same turn about the eye: a look.
            Drag::Fly => self.look(dx, dy),
            // three's perspective pan, at the grabbed depth rather than the
            // target's: one screen height of drag moves the view by the full
            // visible extent there, so the grabbed point stays under the
            // cursor. The target's depth was too fast whenever the target lay
            // beyond what the pointer held.
            Drag::Pan { depth } => {
                let extent = 2.0 * depth * (FOV_Y_DEG.to_radians() / 2.0).tan();
                let forward = (self.camera.target - self.camera.position()).normalize();
                let right = forward.cross(Vec3::Z).normalize();
                let up = right.cross(forward);
                self.camera.target += (right * -dx + up * dy) * (extent / height);
            }
        }
    }

    pub(super) fn viewport_point(&self, point: Point<Pixels>) -> Vec2 {
        Vec2::new(
            f32::from(point.x - self.viewport_origin.x),
            f32::from(point.y - self.viewport_origin.y),
        )
    }

    pub(super) fn viewport_size(&self) -> Vec2 {
        Vec2::new(f32::from(self.size.width), f32::from(self.size.height))
    }

    /// The eye the room is being drawn through — and therefore the one the
    /// builder projects sockets with, so a mark and a pointer are talking
    /// about the same pixel.
    pub(crate) fn camera(&self) -> Camera {
        self.camera
    }
}

/// The pose a rig opens at: [`View::Front`] of its own framing, fitted to the
/// frame it will be drawn into.
pub(super) fn opening_camera(framing: &Framing, view: &Viewfinder) -> Camera {
    Camera::for_view(View::Front, framing, None, view)
}

/// three's `getZoomScale`: exponential in the scroll distance, so ten small
/// notches and one big flick land in the same place. Below one zooms in.
fn zoom_scale(distance: f32) -> f32 {
    ZOOM_BASE.powf(ZOOM_SPEED * distance * 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visualizer::test_support::{empty_pick, visualizer};

    /// Looking while strafing must not invalidate the window: a dirty window
    /// is drawn synchronously on every key repeat, which stepped the fly and
    /// submitted a stage frame off the display's beat. The fly's own frames
    /// still carry the look and the keys.
    #[test]
    fn a_fly_takes_look_and_keys_without_invalidating_the_window() {
        let mut state = visualizer(None, empty_pick(Camera::default()));
        state.size = gpui::size(px(800.), px(600.));
        assert!(state.input_redraws());
        state.begin_drag(Drag::Fly, gpui::point(px(400.), px(300.)));
        assert!(state.fly_key("d", true));
        let start = state.camera;
        let t0 = Instant::now();
        for frame in 1..=10u32 {
            state.dragged(gpui::point(px(3.), px(0.)));
            assert!(!state.input_redraws(), "a look invalidated the window");
            state.fly_tick(t0 + Duration::from_millis(16 * u64::from(frame)), false);
        }
        assert!(
            state.camera.azimuth != start.azimuth,
            "the look did not turn"
        );
        assert!(
            state.camera.position().distance(start.position()) > 0.0,
            "the strafe did not move"
        );
        state.end_drag();
        assert!(state.input_redraws());
    }

    /// A wheel that does not travel leaves the camera and draws no pivot,
    /// and one that does still zooms.
    #[test]
    fn a_still_wheel_leaves_the_camera_alone() {
        let mut state = visualizer(None, empty_pick(Camera::default()));
        state.size = gpui::size(px(800.), px(600.));
        let start = state.camera;
        state.wheel(0.0, gpui::point(px(200.), px(150.)));
        assert_eq!(state.camera, start);
        assert!(state.pivot_dot.is_none(), "a still wheel flashed the pivot");
        state.wheel(-40.0, gpui::point(px(200.), px(150.)));
        assert!(state.camera != start, "a travelling wheel did not zoom");
    }
}
