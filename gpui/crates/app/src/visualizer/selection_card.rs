//! The floating card that edits the selection: where it sits beside the
//! selected objects on screen, and the spring it follows them with.
use super::*;

impl Visualizer {
    pub(super) fn selection_card_position(&mut self, size: gpui::Size<Pixels>) -> Point<Pixels> {
        // A press on the card freezes it: a scrubbed span reshapes the object,
        // and a card that followed would slide out from under the pointer.
        if self.stage.borrow().selection_card_held {
            if let Some(last) = self.selection_card_last {
                return last;
            }
        }
        let viewport = Vec2::new(f32::from(size.width), f32::from(size.height));
        let stage = self.stage.borrow();
        let camera = stage
            .displayed_pick
            .as_ref()
            .map_or(self.camera, |pick| pick.camera);
        let mut lo = Vec2::splat(f32::INFINITY);
        let mut hi = Vec2::splat(f32::NEG_INFINITY);
        if viewport.min_element() > 1.0 {
            for object in self.selection.selected() {
                let (EditorObject::Fixture(id) | EditorObject::StagePiece(id)) = object;
                let bounds = stage
                    .displayed_pick
                    .as_ref()
                    .and_then(|pick| pick.geometry.bounds.get(object).copied())
                    .or_else(|| self.build.as_ref().and_then(|build| build.room_bounds(id)));
                if let Some(bounds) = bounds {
                    for corner in bounds.corners() {
                        if (corner - camera.position()).dot(camera.target - camera.position())
                            <= 0.0
                        {
                            continue;
                        }
                        let ndc = camera.project(corner, viewport.x / viewport.y);
                        let p = Vec2::new(
                            (ndc.x + 1.0) * viewport.x * 0.5,
                            (1.0 - ndc.y) * viewport.y * 0.5,
                        );
                        lo = lo.min(p);
                        hi = hi.max(p);
                    }
                }
            }
        }
        let measured = stage.selection_card_size;
        let card = Vec2::new(
            f32::from(measured.width).max(280.0),
            f32::from(measured.height).max(1.0),
        );
        drop(stage);
        let selected = self.selection.selected();
        let sticky = self
            .selection_card_side
            .as_ref()
            .filter(|(owner, _)| owner.as_slice() == selected)
            .map(|(_, side)| *side);
        let (at, side) = selection_card_at(lo, hi, viewport, card, sticky);
        self.selection_card_side = side.map(|side| (selected.to_vec(), side));
        let at = Point::new(px(at.x), px(at.y));
        self.selection_card_last = Some(at);
        at
    }
}

/// A continuously integrated version of the sidebar spring. Camera updates
/// change its destination without restarting its acceleration from rest.
pub(super) struct SelectionMotion {
    position: Vec2,
    velocity: Vec2,
    target: Vec2,
    pub(super) since: Option<Instant>,
    reduced: bool,
}

impl SelectionMotion {
    pub(super) fn new(reduced: bool) -> Self {
        Self {
            position: Vec2::ZERO,
            velocity: Vec2::ZERO,
            target: Vec2::ZERO,
            since: None,
            reduced,
        }
    }

    pub(super) fn sample(&mut self, target: Vec2, now: Instant) -> Vec2 {
        let Some(previous) = self.since.filter(|_| !self.reduced) else {
            self.position = target;
            self.velocity = Vec2::ZERO;
            self.target = target;
            self.since = Some(now);
            return target;
        };
        let elapsed = now.saturating_duration_since(previous).as_secs_f32();
        let duration = luma_ui::motion::span(&luma_ui::motion::SURFACE).as_secs_f32();
        for axis in 0..2 {
            let (offset, velocity) = luma_ui::motion::ROOT.advance(
                self.position[axis] - self.target[axis],
                self.velocity[axis],
                elapsed,
                duration,
            );
            self.position[axis] = self.target[axis] + offset;
            self.velocity[axis] = velocity;
        }
        self.since = Some(now);
        self.target = target;
        self.position
    }
}

/// Prefer beside the object's silhouette, flip sides when space runs out,
/// and keep the whole scrollable card reachable at viewport edges.
/// Which side of the selection the object card sits on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CardSide {
    Right,
    Left,
    Below,
    Above,
}

/// Place the card beside the selection's screen bounds `lo..hi`.
///
/// A wide object puts the card below or above it, a tall one to its right or
/// left: that is the side whose edge does not move when the object is
/// lengthened. `sticky` is the side chosen last time for the same selection,
/// and it wins while it still fits. Returns the side used, or `None` when
/// nothing is on screen.
fn selection_card_at(
    lo: Vec2,
    hi: Vec2,
    viewport: Vec2,
    card: Vec2,
    sticky: Option<CardSide>,
) -> (Vec2, Option<CardSide>) {
    const GAP: f32 = 16.0;
    let inset = Vec2::splat(12.0);
    let max = (viewport - card - inset).max(inset);
    if !lo.is_finite() || !hi.is_finite() {
        return (Vec2::new(max.x, inset.y), None);
    }
    let center = (lo + hi - card) * 0.5;
    let place = |side: CardSide| match side {
        CardSide::Right => Vec2::new(hi.x + GAP, center.y),
        CardSide::Left => Vec2::new(lo.x - card.x - GAP, center.y),
        CardSide::Below => Vec2::new(center.x, hi.y + GAP),
        CardSide::Above => Vec2::new(center.x, lo.y - card.y - GAP),
    };
    let fits = |side: CardSide| {
        let at = place(side);
        match side {
            CardSide::Right => at.x <= max.x,
            CardSide::Left => at.x >= inset.x,
            CardSide::Below => at.y <= max.y,
            CardSide::Above => at.y >= inset.y,
        }
    };
    let size = hi - lo;
    let order = if size.x > size.y {
        [
            CardSide::Below,
            CardSide::Above,
            CardSide::Right,
            CardSide::Left,
        ]
    } else {
        [
            CardSide::Right,
            CardSide::Left,
            CardSide::Below,
            CardSide::Above,
        ]
    };
    let side = sticky
        .filter(|side| fits(*side))
        .or_else(|| order.into_iter().find(|side| fits(*side)))
        .unwrap_or(if viewport.x - hi.x >= lo.x {
            CardSide::Right
        } else {
            CardSide::Left
        });
    (place(side).clamp(inset, max), Some(side))
}

#[cfg(test)]
mod selection_card_tests {
    use super::*;

    #[test]
    fn motion_uses_the_sidebar_curve_and_retargets_without_jumping() {
        let mut motion = SelectionMotion::new(false);
        let start = Instant::now();
        let duration = luma_ui::motion::span(&luma_ui::motion::SURFACE);
        let first = Vec2::new(600.0, 200.0);
        let flipped = Vec2::new(100.0, 240.0);
        assert_eq!(motion.sample(first, start), first);
        assert_eq!(motion.sample(flipped, start), first);
        let midway = start + duration / 2;
        let expected = first.lerp(flipped, luma_ui::motion::SURFACE.progress(0.5));
        let orbit_target = Vec2::new(130.0, 280.0);
        assert!(motion
            .sample(orbit_target, midway)
            .abs_diff_eq(expected, 1e-3));
        assert!(motion
            .sample(orbit_target, midway + duration * 3)
            .abs_diff_eq(orbit_target, 1e-3));
        motion.since = None;
        assert_eq!(motion.sample(first, midway + duration * 3), first);
    }

    #[test]
    fn jittering_targets_preserve_velocity_and_follow_without_stalling() {
        let mut motion = SelectionMotion::new(false);
        let start = Instant::now();
        motion.sample(Vec2::ZERO, start);
        motion.sample(Vec2::splat(500.0), start);
        for frame in 1..=30 {
            let now = start + Duration::from_millis(frame * 16);
            let target = Vec2::splat(if frame % 2 == 0 { 490.0 } else { 510.0 });
            let position = motion.sample(target, now);
            let velocity = motion.velocity;
            // An arbitrarily sharp reversal changes neither position nor
            // velocity at that instant; only subsequent acceleration changes.
            assert_eq!(motion.sample(-target, now), position);
            assert_eq!(motion.velocity, velocity);
            motion.sample(target, now);
        }
        assert!(
            motion.position.x > 450.0,
            "tracking stalled: {:?}",
            motion.position
        );
    }

    #[test]
    fn spring_tracking_is_independent_of_frame_partition() {
        let start = Instant::now();
        let mut whole = SelectionMotion::new(false);
        let mut frames = SelectionMotion::new(false);
        for motion in [&mut whole, &mut frames] {
            motion.sample(Vec2::ZERO, start);
            motion.sample(Vec2::splat(500.0), start);
        }
        let expected = whole.sample(Vec2::splat(500.0), start + Duration::from_millis(160));
        for frame in 1..=10 {
            frames.sample(
                Vec2::splat(500.0),
                start + Duration::from_millis(frame * 16),
            );
        }
        assert!(frames.position.abs_diff_eq(expected, 1e-3));
        assert!(frames.velocity.abs_diff_eq(whole.velocity, 1e-2));
    }

    #[test]
    fn reduced_motion_tracks_the_object_immediately() {
        let mut motion = SelectionMotion::new(true);
        let now = Instant::now();
        motion.sample(Vec2::ZERO, now);
        assert_eq!(
            motion.sample(Vec2::new(300.0, 200.0), now),
            Vec2::new(300.0, 200.0)
        );
    }

    #[test]
    fn card_follows_the_selection_and_flips_at_the_right_edge() {
        let viewport = Vec2::new(1200.0, 800.0);
        let card = Vec2::new(280.0, 230.0);
        let (right, side) = selection_card_at(
            Vec2::new(400.0, 300.0),
            Vec2::new(500.0, 400.0),
            viewport,
            card,
            None,
        );
        assert_eq!((right.x, side), (516.0, Some(CardSide::Right)));
        let (left, side) = selection_card_at(
            Vec2::new(900.0, 300.0),
            Vec2::new(1000.0, 400.0),
            viewport,
            card,
            None,
        );
        assert_eq!((left.x, side), (604.0, Some(CardSide::Left)));
        assert_eq!(left.y, right.y);
        let (edge, _) = selection_card_at(
            Vec2::new(-100.0, 790.0),
            Vec2::new(1400.0, 1000.0),
            viewport,
            card,
            None,
        );
        assert!(edge.x >= 12.0 && edge.x <= 908.0);
        assert!(edge.y >= 12.0 && edge.y <= 558.0);
    }

    #[test]
    fn a_wide_object_puts_the_card_below_and_keeps_it_there() {
        let viewport = Vec2::new(1200.0, 800.0);
        let card = Vec2::new(280.0, 230.0);
        let (at, side) = selection_card_at(
            Vec2::new(300.0, 200.0),
            Vec2::new(700.0, 240.0),
            viewport,
            card,
            None,
        );
        assert_eq!(side, Some(CardSide::Below));
        assert_eq!(at.y, 256.0);
        // Lengthened until it is taller than wide: the side does not flip.
        let (grown, side) = selection_card_at(
            Vec2::new(300.0, 200.0),
            Vec2::new(340.0, 300.0),
            viewport,
            card,
            side,
        );
        assert_eq!(side, Some(CardSide::Below));
        assert_eq!(grown.y, 316.0);
    }
}

#[cfg(test)]
mod placement_tests {
    use super::*;
    use crate::visualizer::test_support::visualizer;

    #[test]
    fn panel_and_focus_use_the_rendered_objects_world_bounds_once() {
        let camera = Camera {
            target: Vec3::new(1.0, -2.0, 3.0),
            ..Default::default()
        };
        let scene = scene_desc::Scene {
            id: "bounds".into(),
            times: vec![0.0],
            editing: true,
            aim_arrows: false,
            camera: scene_desc::CameraPose {
                position: coords::three_from_world(camera.position()).to_array(),
                target: coords::three_from_world(camera.target).to_array(),
            },
            render: scene_desc::RenderSettings::dark_stage(50.0, 0.5),
            selected_fixture_ids: Vec::new(),
            editor: scene_desc::Editor {
                selected_piece_ids: vec!["deck".into()],
                ..Default::default()
            },
            fixtures: Vec::new(),
            state: std::collections::BTreeMap::new(),
            pieces: vec![scene_desc::Piece {
                id: "deck".into(),
                geometry: scene_desc::Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                kind: "floor".into(),
                pos: [1.0, 2.0, 3.0],
                rot: [0.0, 0.0, 0.6],
                scale: 1.0,
            }],
        };
        let mut library = assets::Library::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"),
        );
        let frame = luma_render::build_frame_with(
            &scene,
            &Default::default(),
            &|_, _| None,
            0.0,
            &mut library,
        )
        .unwrap();
        let object = EditorObject::StagePiece("deck".into());
        // The rendered object: every vertex the frame draws for the deck.
        let expected = Aabb::from_points(
            frame
                .draws
                .iter()
                .filter(|draw| draw.editor_object.as_ref() == Some(&object))
                .flat_map(|draw| {
                    frame.meshes[draw.mesh]
                        .vertices
                        .iter()
                        .map(|vertex| draw.model.transform_point3(Vec3::from(vertex.position)))
                }),
        );
        assert!(!expected.is_empty(), "the frame draws the deck");
        let cage = frame
            .overlays
            .iter()
            .find(|overlay| frame.meshes[overlay.mesh].key.starts_with("::piece-cage:"))
            .unwrap();
        let cage_bounds = Aabb::from_points(
            frame.meshes[cage.mesh]
                .vertices
                .iter()
                .map(|vertex| cage.model.transform_point3(Vec3::from(vertex.position))),
        );
        assert!(
            cage_bounds.min.cmple(expected.min).all() && cage_bounds.max.cmpge(expected.max).all(),
            "the selection cage {cage_bounds:?} must enclose the object {expected:?}"
        );
        let pick = PickSnapshot::from_frame(&frame, &scene, camera, &mut PickCache::default());
        let actual = pick.geometry.bounds[&object];
        assert!(
            actual.min.abs_diff_eq(expected.min, 1e-4),
            "{actual:?} != {expected:?}"
        );
        assert!(actual.max.abs_diff_eq(expected.max, 1e-4));
        let mut state = visualizer(None, pick);
        state.selection.replace([object]);
        state.stage.borrow_mut().selection_card_size = gpui::size(px(280.0), px(80.0));
        let at = state.selection_card_position(state.size);
        let points: Vec<_> = expected
            .corners()
            .map(|corner| camera.project(corner, 1.5))
            .into_iter()
            .collect();
        let top = points
            .iter()
            .map(|p| (1.0 - p.y) * 400.0)
            .fold(f32::INFINITY, f32::min);
        let bottom = points
            .iter()
            .map(|p| (1.0 - p.y) * 400.0)
            .fold(f32::NEG_INFINITY, f32::max);
        let left = points
            .iter()
            .map(|p| (p.x + 1.0) * 600.0)
            .fold(f32::INFINITY, f32::min);
        let right = points
            .iter()
            .map(|p| (p.x + 1.0) * 600.0)
            .fold(f32::NEG_INFINITY, f32::max);
        // Beside the object the card centers on it vertically; below or above
        // it, horizontally. Either way it is placed from the rendered bounds.
        let beside = (f32::from(at.y) + 40.0 - (top + bottom) * 0.5).abs() < 1e-3;
        let under = (f32::from(at.x) + 140.0 - (left + right) * 0.5).abs() < 1e-3;
        assert!(beside || under, "{at:?} is not aligned to the object");
        assert!(state.focus_selection());
        for corner in expected.corners() {
            let ndc = state.camera.project(corner, 1.5);
            assert!(
                ndc.x.abs() < 1.0 && ndc.y.abs() < 1.0,
                "F clipped the selected object: {ndc:?}"
            );
        }
        assert!(
            state.camera.target.z > 3.0,
            "F must frame the raised object, not the floor"
        );
    }
}
