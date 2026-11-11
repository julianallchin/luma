//! The editor's pointer gestures on the room: click and marquee selection,
//! gizmo drags, and the data-space poses they preview and commit.
use super::*;

/// What a finished press owes the app — see [`Visualizer::editor_release`].
pub(crate) enum ReleaseAct {
    /// A click while the hand was placing: aim there and drop.
    Place(Point<Pixels>),
    /// A gizmo drag ended on these stage pieces: write their previewed poses
    /// into the graph.
    CommitPose(Vec<String>),
}

pub(super) enum EditorDrag {
    ClickOrbit {
        gesture: ClickOrbit,
        shift: bool,
        start: Vec2,
    },
    Marquee(Marquee),
    Gizmo {
        handle: GizmoHandle,
        start: Vec2,
        /// The widget's world pivot, as it stood when the press landed. Held
        /// rather than re-read: the live one moves with the objects, and a
        /// rotation of the press-time poses about a pivot that has since
        /// travelled is a different rotation every frame.
        pivot: Vec3,
        camera: Camera,
        space: luma_scene::gizmo::GizmoSpace,
        originals: Vec<(EditorObject, Vec3, Quat)>,
    },
}

impl Visualizer {
    /// Replace the selection outright — the redistribute round trip lands new
    /// fixture ids and the old ones no longer exist to keep.
    pub(crate) fn replace_selection(&mut self, targets: impl IntoIterator<Item = EditorObject>) {
        self.selection.replace(targets);
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Every selected object, primary last.
    pub(crate) fn selected_objects(&self) -> &[EditorObject] {
        self.selection.selected()
    }

    /// The live marquee rectangle, in viewport pixels.
    pub(super) fn marquee_rect(&self) -> Option<luma_scene::ScreenRect> {
        match &self.editor_drag {
            Some(EditorDrag::Marquee(marquee)) => Some(marquee.rect()),
            _ => None,
        }
    }

    /// Replace the selection with one object — the headless pick's write.
    pub(crate) fn select_object(&mut self, object: EditorObject) {
        self.selection.click(object, false);
    }

    /// The pointer's ray through the stage pane, off the camera alone — the
    /// aim a headless run has, where the displayed frame's own `PickSnapshot`
    /// does not exist. World space, unit direction not guaranteed.
    pub(crate) fn stage_pick_ray(&self, at: Point<Pixels>) -> Option<luma_scene::Ray> {
        let pane = self.stage.borrow().pane;
        let point = Vec2::new(
            f32::from(at.x - pane.origin.x),
            f32::from(at.y - pane.origin.y),
        );
        let viewport = Vec2::new(f32::from(pane.size.width), f32::from(pane.size.height));
        if viewport.x <= 1.0 || viewport.y <= 1.0 {
            return None;
        }
        Some(self.camera.pixel_ray(point, viewport))
    }

    pub(super) fn editor_press(&mut self, point: Point<Pixels>, shift: bool) {
        let at = self.viewport_point(point);
        if self.presentation {
            self.editor_drag = Some(EditorDrag::ClickOrbit {
                gesture: ClickOrbit::new(at),
                shift: false,
                start: at,
            });
            return;
        }
        let viewport = self.viewport_size();
        // A hand in the air owns the click's *meaning* (place, or nothing),
        // and the gizmo must not intercept it — but the gesture is still a
        // ClickOrbit, so a left drag mid-placement orbits the camera.
        let placing = self
            .build
            .as_ref()
            .is_some_and(|build| build.hand.owns_pointer());
        let stage = self.stage.borrow();
        let Some(pick) = stage.displayed_pick.as_ref().filter(|_| !placing) else {
            drop(stage);
            self.editor_drag = Some(EditorDrag::ClickOrbit {
                gesture: ClickOrbit::new(at),
                shift,
                start: at,
            });
            return;
        };
        if let Some(pivot) = pick.gizmo_pivot {
            let scale = gizmo_scale(
                (pick.camera.position() - pivot).length(),
                pick.camera.fov_y_deg,
            );
            if let Some(hit) = pick.gizmo_space.hit(
                pick.ray(at, viewport),
                pivot,
                scale,
                pick.camera.position() - pivot,
                self.gizmo_mode,
            ) {
                // What the widget may actually grab: whatever its joint
                // leaves free — a bolted piece's pose is a relation, and axes
                // over a relation is a widget that lies.
                let build = self.build.as_ref();
                let originals: Vec<_> = self
                    .selection
                    .selected()
                    .iter()
                    .filter(|object| match object {
                        EditorObject::Fixture(id) => {
                            build.is_none_or(|build| build.gizmo_space(id).is_some())
                        }
                        EditorObject::StagePiece(id) => {
                            build.is_some_and(|build| build.gizmo_space(id).is_some())
                        }
                    })
                    .filter_map(|object| {
                        let pose = stage
                            .scene
                            .as_ref()
                            .and_then(|scene| object_pose(scene, object))?;
                        Some((object.clone(), pose.position, pose.rotation))
                    })
                    .collect();
                // With no movable targets, the press remains a camera gesture.
                if !originals.is_empty() {
                    // The grabbed handle stays lit for the whole drag: the
                    // hover is only recomputed on unbuttoned moves.
                    self.gizmo_hover = Some(hit.handle);
                    self.editor_drag = Some(EditorDrag::Gizmo {
                        handle: hit.handle,
                        start: at,
                        pivot,
                        camera: pick.camera,
                        space: pick.gizmo_space,
                        originals,
                    });
                    return;
                }
            }
        }
        self.editor_drag = Some(EditorDrag::ClickOrbit {
            gesture: ClickOrbit::new(at),
            shift,
            start: at,
        });
    }

    /// The gizmo handle under the pointer, tested against the frame on
    /// screen — the same cast [`Self::editor_press`] makes, asked on every
    /// unbuttoned move so the widget lights the handle *before* it is
    /// grabbed. Analytic against a handful of primitives, so it is cheap
    /// enough to ask per move.
    pub(super) fn hover_gizmo(&self, point: Point<Pixels>) -> Option<GizmoHandle> {
        if self.presentation {
            return None;
        }
        let at = self.viewport_point(point);
        let viewport = self.viewport_size();
        let stage = self.stage.borrow();
        let pick = stage.displayed_pick.as_ref()?;
        let pivot = pick.gizmo_pivot?;
        let scale = gizmo_scale(
            (pick.camera.position() - pivot).length(),
            pick.camera.fov_y_deg,
        );
        pick.gizmo_space
            .hit(
                pick.ray(at, viewport),
                pivot,
                scale,
                pick.camera.position() - pivot,
                self.gizmo_mode,
            )
            .map(|hit| hit.handle)
    }

    pub(super) fn selection_gizmo_space(&self) -> luma_scene::gizmo::GizmoSpace {
        if self.presentation || self.selection.selected().is_empty() {
            return luma_scene::gizmo::GizmoSpace::DISABLED;
        }
        if let Some(build) = self.build.as_ref() {
            let mut spaces = self.selection.selected().iter().map(|object| match object {
                EditorObject::StagePiece(id) | EditorObject::Fixture(id) => build.gizmo_space(id),
            });
            spaces
                .next()
                .flatten()
                .filter(|space| spaces.all(|next| next == Some(*space)))
                .unwrap_or(luma_scene::gizmo::GizmoSpace::DISABLED)
        } else {
            luma_scene::gizmo::GizmoSpace::default()
        }
    }

    /// Switch which transform widget the selection wears — the toolbar's two
    /// segments and the stage page's `W`/`E` land here together.
    pub(crate) fn set_gizmo_mode(&mut self, mode: GizmoMode) {
        self.gizmo_mode = mode;
        self.gizmo_hover = None;
    }

    pub(super) fn editor_moved(&mut self, point: Point<Pixels>) {
        let at = self.viewport_point(point);
        let viewport = self.viewport_size();
        let Some(mut interaction) = self.editor_drag.take() else {
            return;
        };
        match &mut interaction {
            EditorDrag::ClickOrbit {
                gesture,
                shift,
                start,
            } => {
                if *shift {
                    let mut marquee = Marquee::new(*start);
                    marquee.moved(at);
                    if marquee.qualifies() {
                        interaction = EditorDrag::Marquee(marquee);
                    }
                } else {
                    let update = gesture.moved(at);
                    match update {
                        ClickOrbitUpdate::Pending => {}
                        ClickOrbitUpdate::BeginOrbit(delta) | ClickOrbitUpdate::Orbit(delta) => {
                            // The pivot is picked once, where the press
                            // landed, and held for the whole drag.
                            let pivot = match (update, self.drag) {
                                (ClickOrbitUpdate::Orbit(_), Some((Drag::Orbit { pivot }, _))) => {
                                    pivot
                                }
                                _ => self.pivot_at(*start),
                            };
                            self.drag = Some((Drag::Orbit { pivot }, point));
                            self.dragged(Point::new(px(delta.x), px(delta.y)));
                        }
                    }
                }
            }
            EditorDrag::Marquee(marquee) => marquee.moved(at),
            gizmo @ EditorDrag::Gizmo { .. } => self.apply_gizmo(gizmo, at, viewport),
        }
        self.editor_drag = Some(interaction);
    }

    /// Ends the press. What comes back is the verb the release owes — a
    /// placement click, or a pose commit for dragged pieces — which the
    /// caller runs on [`Luma`], because writing the graph is a verb and not a
    /// camera gesture.
    pub(super) fn editor_release(&mut self, point: Point<Pixels>) -> Option<ReleaseAct> {
        let at = self.viewport_point(point);
        let viewport = self.viewport_size();
        // A release ends the camera drag whatever else this function decides:
        // the selection paths below can bail before reaching the end. A fly
        // is the right button's, and outlives a left release.
        if self.fly.is_none() {
            self.drag = None;
        }
        let interaction = self.editor_drag.take()?;
        if self.presentation {
            return None;
        }
        // Set only by a plain click on a fixture — the gesture that selects a
        // row. See the expansion at the tail.
        let mut clicked_fixture: Option<String> = None;
        // Only the placing hand's click is a drop. A run's controls are its
        // own card, so a click on the room mid-extend aims nothing.
        let placing = self
            .build
            .as_ref()
            .is_some_and(|build| build.hand.aims_with_pointer());
        let holding = self
            .build
            .as_ref()
            .is_some_and(|build| build.hand.owns_pointer());
        if let EditorDrag::ClickOrbit { ref gesture, .. } = interaction {
            if gesture.clone().released() == ClickOrbitRelease::Click && holding {
                return placing.then_some(ReleaseAct::Place(point));
            }
        }
        let stage = self.stage.borrow();
        let Some(pick) = stage.displayed_pick.as_ref() else {
            return None;
        };
        match interaction {
            EditorDrag::ClickOrbit { gesture, shift, .. }
                if gesture.released() == ClickOrbitRelease::Click =>
            {
                if let Some(target) = pick.pick(at, viewport) {
                    if !shift {
                        if let EditorObject::Fixture(id) = &target {
                            clicked_fixture = Some(id.clone());
                        }
                    }
                    self.selection.click(target, shift);
                } else if !shift {
                    self.selection.clear();
                }
            }
            EditorDrag::Marquee(marquee) => self.selection.replace(pick.marquee(marquee, viewport)),
            // A finished gizmo drag in a venue being built owes the graph its
            // poses: the drag previewed by writing the scene, and the scene is
            // not where a node's position lives.
            EditorDrag::Gizmo { originals, .. } => {
                let nodes: Vec<String> = originals
                    .iter()
                    .filter(|_| self.build.is_some())
                    .map(|(object, _, _)| match object {
                        EditorObject::StagePiece(id) | EditorObject::Fixture(id) => id.clone(),
                    })
                    .collect();
                return (!nodes.is_empty()).then_some(ReleaseAct::CommitPose(nodes));
            }
            _ => return None,
        }
        // The builder selects the *node*, not the render object: a subtree and a
        // trim are things the graph has names for — and the
        // render object's identity IS the node id, which is what lets the two
        // agree by construction. After *every* gesture that can move the
        // selection — click and marquee alike — so the builder never holds a
        // second, staler answer to "what is selected".
        // A fixture clicked out of a row selects the row: the fixtures of one
        // model chained on the same face are one thing to a rigger, and the
        // card that follows edits them as one. A marquee is deliberate
        // multi-select and is left exactly as swept.
        let expanded: Vec<String> = clicked_fixture
            .as_deref()
            .map(|id| {
                let stage = self.stage.borrow();
                let path_of = |node: &str| {
                    stage.scene.as_ref().and_then(|scene| {
                        scene
                            .fixtures
                            .iter()
                            .find(|fixture| fixture.id == node)
                            .map(|fixture| fixture.fixture_path.clone())
                    })
                };
                self.build
                    .as_ref()
                    .map(|build| build.distribution_of(id, &path_of))
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        if expanded.len() > 1 {
            if let Some(primary) = clicked_fixture.clone() {
                let mut ordered: Vec<String> = expanded
                    .iter()
                    .filter(|id| **id != primary)
                    .cloned()
                    .collect();
                ordered.push(primary);
                self.selection
                    .replace(ordered.into_iter().map(EditorObject::Fixture));
            }
        }
        if let Some(build) = self.build.as_mut() {
            let primary = self.selection.primary().map(|object| match object {
                EditorObject::Fixture(id) | EditorObject::StagePiece(id) => id.clone(),
            });
            build.select(primary);
            build.distribution = expanded;
        }
        None
    }

    /// Update only transient scene positions; the builder owns persistence on release.
    pub(crate) fn preview_stage_positions(&mut self, positions: &[(String, [f64; 3])]) {
        let mut stage = self.stage.borrow_mut();
        let Some(scene) = stage.scene.as_mut() else {
            return;
        };
        for (id, position) in positions {
            let position = position.map(|value| value as f32);
            if let Some(piece) = scene.pieces.iter_mut().find(|piece| &piece.id == id) {
                piece.pos = position;
            }
            if let Some(fixture) = scene.fixtures.iter_mut().find(|fixture| &fixture.id == id) {
                fixture.pos = position;
            }
        }
        // Position changes are intentionally absent from IdleKey. Numeric drags
        // do not set editor_drag, so invalidate the cached image explicitly.
        stage.idle = None;
    }

    /// One dragged piece's or fixture's current *previewed* pose, in the socket layer's
    /// frame — what the gizmo wrote into the scene, read back so the commit
    /// inverts exactly what is on screen.
    pub(crate) fn piece_pose_three(&self, id: &str) -> Option<glam::DMat4> {
        let stage = self.stage.borrow();
        let scene = stage.scene.as_ref()?;
        let (pos, rot) = scene
            .pieces
            .iter()
            .find(|piece| piece.id == id)
            .map(|piece| (piece.pos, piece.rot))
            .or_else(|| {
                scene
                    .fixtures
                    .iter()
                    .find(|fixture| fixture.id == id)
                    .map(|fixture| (fixture.pos, fixture.rot))
            })?;
        Some(coords::three_pose_from_data(pos, rot).as_dmat4())
    }

    /// The viewport's own laid-out bounds. Laid out — and recorded — whether
    /// or not a renderer exists, which is what a headless aim depends on.
    pub(crate) fn stage_pane(&self) -> Bounds<Pixels> {
        self.stage.borrow().pane
    }

    pub(crate) fn prepare_presentation(&mut self) {
        self.settings = Default::default();
        self.end_drag();
        // A shortcut can arrive before mouse-up. Cancel an unfinished pose
        // preview before parking the editor, so it cannot survive without a commit.
        if let Some(EditorDrag::Gizmo { originals, .. }) = self.editor_drag.take() {
            let mut stage = self.stage.borrow_mut();
            if let Some(scene) = stage.scene.as_mut() {
                for (object, position, rotation) in originals {
                    set_object_pose(scene, &object, position, rotation);
                }
            }
            stage.idle = None;
        }
        self.gizmo_hover = None;
    }

    /// Where the pointer is aiming in the room, in the socket layer's frame:
    /// the mesh face under it when a frame is on screen, the floor plane when
    /// none is — which is the state a headless run is always in.
    pub(crate) fn stage_cursor(
        &self,
        at: Point<Pixels>,
    ) -> Option<(glam::DVec3, Option<crate::stage::hand::SurfaceHit>)> {
        // The pane's own bounds, not the canvas's: the pane is laid out — and
        // recorded — whether or not a renderer exists, and it is the surface
        // the beads and marks are projected against, so aiming through it is
        // what keeps a pointer and a bead talking about the same pixel.
        let pane = self.stage.borrow().pane;
        let point = Vec2::new(
            f32::from(at.x - pane.origin.x),
            f32::from(at.y - pane.origin.y),
        );
        let viewport = Vec2::new(f32::from(pane.size.width), f32::from(pane.size.height));
        if viewport.x <= 1.0 || viewport.y <= 1.0 {
            return None;
        }
        let stage = self.stage.borrow();
        if let Some(pick) = stage.displayed_pick.as_ref() {
            let ray = pick.ray(point, viewport);
            let hit = pick
                .geometry
                .graph
                .raycast(ray, Default::default(), pick)
                .into_iter()
                .find_map(|hit| {
                    match pick.geometry.objects.get(hit.node.0 as usize)?.as_ref()? {
                        EditorObject::StagePiece(id) => Some(crate::stage::hand::SurfaceHit {
                            piece: id.clone(),
                            point: coords::three_from_world(hit.point).as_dvec3(),
                            normal: coords::three_from_world(hit.face_normal)
                                .as_dvec3()
                                .normalize_or_zero(),
                        }),
                        EditorObject::Fixture(_) => None,
                    }
                });
            if let Some(hit) = hit {
                return Some((hit.point, Some(hit)));
            }
            return crate::stage::hand::floor_point(&ray).map(|world| (world, None));
        }
        crate::stage::hand::floor_point(&self.camera.pixel_ray(point, viewport))
            .map(|world| (world, None))
    }

    fn apply_gizmo(&mut self, drag: &EditorDrag, end: Vec2, viewport: Vec2) {
        let EditorDrag::Gizmo {
            handle,
            start,
            pivot,
            camera,
            space,
            originals,
        } = drag
        else {
            return;
        };
        let Some((translation, rotation)) =
            luma_scene::gizmo::drag_delta(*handle, *camera, *start, end, *pivot, viewport, *space)
        else {
            return;
        };
        let mut stage = self.stage.borrow_mut();
        let Some(scene) = stage.scene.as_mut() else {
            return;
        };
        // Every target rotates about the widget's own pivot: a rotation about a
        // point the operator cannot see is a rotation they did not ask for.
        let targets: Vec<_> = originals
            .iter()
            .map(|(_, position, rotation)| TransformTarget {
                position: *position,
                rotation: *rotation,
                anchor: *position,
            })
            .collect();
        for ((object, _, _), target) in originals.iter().zip(targets) {
            let changed = apply_translation(
                apply_rotation(target, rotation, *pivot, PivotMode::Group),
                translation,
            );
            set_object_pose(scene, object, changed.position, changed.rotation);
        }
    }
}

/// Data space to the world the renderer draws in, and back.
///
/// [`coords::three_pose_from_data`] is the whole conversion; this is the
/// rotation that takes its result the last step, exactly as `build_frame_with`
/// does for every draw. Spelled here so the two functions below are the only
/// place in this screen that knows a stored triple is not a world pose.
fn to_world() -> Mat4 {
    Mat4::from_mat3(coords::three_to_world_basis())
}

/// Where an editor object *is*, in the renderer's world space.
///
/// This and [`set_object_pose`] are the single data↔world boundary for the
/// editor: everything a gizmo touches — the camera, `Draw::model`, the
/// overlays, a drag delta measured on screen — is world space, and a stored
/// pose is neither that nor a rotation away from it. The swap is a mirror
/// (`coords`, module note), so a pose that skips it lands at `(x, -y, z)`:
/// mirrored across the room, and moving the wrong way when dragged.
pub(super) fn object_pose(
    scene: &scene_desc::Scene,
    object: &EditorObject,
) -> Option<TransformTarget> {
    let (pos, rot) = match object {
        EditorObject::Fixture(id) => scene
            .fixtures
            .iter()
            .find(|object| &object.id == id)
            .map(|object| (object.pos, object.rot)),
        EditorObject::StagePiece(id) => scene
            .pieces
            .iter()
            .find(|object| &object.id == id)
            .map(|object| (object.pos, object.rot)),
    }?;
    let (_, rotation, position) =
        (to_world() * coords::three_pose_from_data(pos, rot)).to_scale_rotation_translation();
    Some(TransformTarget {
        position,
        rotation,
        anchor: position,
    })
}

/// Write a world pose back to the stored triple — the inverse of
/// [`object_pose`], through the same conversion.
///
/// For a stage piece this write is the drag's *preview* only: its pose is
/// derived from its joint, so the release inverts the mate and writes the
/// graph (`Luma::stage_commit_pose`), and the re-solve then overwrites what
/// was previewed here with the same numbers read back.
fn set_object_pose(
    scene: &mut scene_desc::Scene,
    object: &EditorObject,
    position: Vec3,
    rotation: Quat,
) {
    let (pos, rot) = coords::data_pose_of(
        to_world().inverse() * Mat4::from_rotation_translation(rotation, position),
    );
    match object {
        EditorObject::Fixture(id) => {
            if let Some(object) = scene.fixtures.iter_mut().find(|object| &object.id == id) {
                object.pos = pos;
                object.rot = rot;
            }
        }
        EditorObject::StagePiece(id) => {
            if let Some(object) = scene.pieces.iter_mut().find(|object| &object.id == id) {
                object.pos = pos;
                object.rot = rot;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visualizer::test_support::{empty_pick, visualizer};

    #[test]
    fn height_preview_invalidates_a_settled_viewport_before_release() {
        let camera = Camera::default();
        let pick = empty_pick(camera);
        let mut view = visualizer(None, pick);
        let controls = view.render_controls.clone();
        let key = || IdleKey {
            time_bits: 0.0_f32.to_bits(),
            camera,
            size: (1200, 800),
            controls: controls.clone(),
            selected: Vec::new(),
            selected_pieces: vec!["deck".into()],
            gizmo_mode: Default::default(),
            gizmo_hover: None,
            universe: None,
            display: luma_render::DisplayRange::Sdr,
        };
        view.stage.borrow_mut().scene = Some(scene_desc::Scene {
            id: "height-preview".into(),
            times: vec![0.0],
            editing: true,
            aim_arrows: false,
            camera: scene_desc::CameraPose {
                position: [4.0, 3.0, 5.0],
                target: [0.0; 3],
            },
            render: scene_desc::RenderSettings::dark_stage(50.0, 0.5),
            selected_fixture_ids: Vec::new(),
            editor: Default::default(),
            state: BTreeMap::new(),
            pieces: vec![scene_desc::Piece {
                id: "deck".into(),
                geometry: scene_desc::Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                kind: "floor".into(),
                pos: [0.0; 3],
                rot: [0.0; 3],
                scale: 1.0,
            }],
            fixtures: vec![scene_desc::Fixture {
                id: "child".into(),
                fixture_path: "Luma/Mover.qxf".into(),
                mode_name: "Default".into(),
                pos: [0.0, 0.0, 1.0],
                rot: [0.0; 3],
                aim: [0.0; 2],
            }],
        });
        // Numeric scrubbing has neither a camera drag nor a gizmo drag to
        // hold the idle gate open. Each intermediate position must owe a frame.
        for height in [0.1, 0.2, 0.3] {
            view.stage.borrow_mut().idle = Some((key(), 0));
            assert!(view.editor_drag.is_none() && view.drag.is_none());
            view.preview_stage_positions(&[
                ("deck".into(), [0.0, 0.0, height]),
                ("child".into(), [0.0, 0.0, height + 1.0]),
            ]);
            let stage = view.stage.borrow();
            assert!(
                stage.idle.is_none(),
                "preview reused the image from before the drag"
            );
            let scene = stage.scene.as_ref().unwrap();
            assert_eq!(scene.pieces[0].pos[2], height as f32);
            assert_eq!(scene.fixtures[0].pos[2], (height + 1.0) as f32);
        }
    }

    #[test]
    fn fullscreen_clicks_and_shift_drags_preserve_selection() {
        let camera = Camera::default();
        let pick = empty_pick(camera);
        let mut state = visualizer(None, pick);
        let selected = EditorObject::Fixture("selected".into());
        state.selection.replace([selected.clone()]);
        state.prepare_presentation();
        state.presentation = true;
        assert_eq!(state.camera, camera);
        assert_eq!(
            state.selection_gizmo_space(),
            luma_scene::gizmo::GizmoSpace::DISABLED
        );
        let start = gpui::point(px(100.), px(100.));
        state.editor_press(start, false);
        assert!(state.editor_release(start).is_none());
        assert_eq!(state.selection.selected(), std::slice::from_ref(&selected));
        state.editor_press(start, true);
        let end = gpui::point(px(200.), px(150.));
        state.editor_moved(end);
        assert_ne!(state.camera, camera, "Shift-drag should still orbit");
        assert!(state.editor_release(end).is_none());
        assert_eq!(state.selection.selected(), &[selected]);
    }

    #[test]
    fn releasing_a_camera_orbit_preserves_the_selected_distribution() {
        use luma_lib::models::venue_graph::{VenueGraphRows, VenueNode};
        let rig = crate::library::Rig {
            fixtures: Vec::new(),
            venue: Default::default(),
            definitions: HashMap::new(),
            environment: Default::default(),
            haze: Default::default(),
            lasers: Default::default(),
            rows: VenueGraphRows {
                nodes: vec![VenueNode {
                    id: "venue".into(),
                    venue_id: "venue".into(),
                    kind: "venue".into(),
                    catalog_ref: None,
                    label: None,
                    placement: None,
                }],
                ..Default::default()
            },
        };
        let sockets = luma_scene::catalog::VenueSockets::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"),
            Arc::new(luma_scene::catalog::NoFixtures),
        )
        .unwrap();
        let mut build = crate::stage::Build::new("venue", &rig, sockets).unwrap();
        build.selected = Some("first".into());
        build.distribution = vec!["first".into(), "second".into()];
        let camera = Camera::default();
        let pick = empty_pick(camera);
        let mut state = visualizer(Some(build), pick);
        state.selection.replace([
            EditorObject::Fixture("second".into()),
            EditorObject::Fixture("first".into()),
        ]);
        state.editor_press(gpui::point(px(100.0), px(100.0)), false);
        state.editor_moved(gpui::point(px(200.0), px(150.0)));
        assert_ne!(state.camera, camera);
        assert!(state
            .editor_release(gpui::point(px(200.0), px(150.0)))
            .is_none());
        let build = state.build.as_ref().unwrap();
        assert_eq!(build.selected.as_deref(), Some("first"));
        assert_eq!(build.distribution, ["first", "second"]);
        assert_eq!(state.selection.selected().len(), 2);
    }
}
