//! Hit-testing: the CPU geometry a ray is cast against, snapshotted per
//! submitted frame and paired back to the pixels it describes by serial.
use super::*;

/// The hit-test geometry itself: what a ray can hit and which authored object
/// each hit names.
///
/// Separated from [`PickSnapshot`] because it is the expensive half and the
/// half that almost never changes. A playing score moves colour through a
/// static rig: the same meshes at the same transforms, frame after frame.
/// Rebuilding this per frame cost more than the whole rest of the UI thread's
/// share of a frame, so it is built once and shared by every snapshot whose
/// draw list still matches [`Self::draws`].
pub(super) struct PickGeometry {
    pub(super) graph: SceneGraph,
    pub(super) meshes: Vec<Arc<TriMesh>>,
    /// Frame node → the authored object it draws. Identity is the *authored
    /// id*, never the node index: an index names whatever inherited the slot
    /// after a re-solve, so a selection held by index could point at some
    /// other object.
    pub(super) objects: Vec<Option<EditorObject>>,
    pub(super) ordered: Vec<EditorObject>,
    /// Where each object is, for the marquee to test against.
    pub(super) anchors: HashMap<EditorObject, Vec3>,
    /// Geometry bounds in camera world space, including every draw of an object.
    pub(super) bounds: HashMap<EditorObject, Aabb>,
    /// The mesh set this was built against, by key: a mesh index means nothing
    /// on its own, and a re-banked frame can keep an index while changing what
    /// it points at.
    pub(super) mesh_keys: Vec<String>,
    /// The picked draws this was built from, in order — mesh, transform and
    /// the object the draw names. Every input the build reads is in here, so
    /// an equal list is the same geometry.
    pub(super) draws: Vec<(usize, Mat4, EditorObject)>,
}

impl PickGeometry {
    /// Whether `frame` would build exactly this geometry again.
    ///
    /// Compared in place against the stored draw list rather than by building
    /// a fresh key: the whole point is to touch no allocator on the frames
    /// that match, which is nearly all of them.
    fn matches(&self, frame: &luma_render::Frame) -> bool {
        if self.mesh_keys.len() != frame.meshes.len()
            || !self
                .mesh_keys
                .iter()
                .zip(&frame.meshes)
                .all(|(key, mesh)| key == &mesh.key)
        {
            return false;
        }
        let mut resident = self.draws.iter();
        for draw in picked_draws(frame) {
            let Some(object) = draw.editor_object.as_ref() else {
                continue;
            };
            let Some((mesh, model, was)) = resident.next() else {
                return false;
            };
            if *mesh != draw.mesh || *model != draw.model || was != object {
                return false;
            }
        }
        resident.next().is_none()
    }
}

/// The opaque draws a ray can hit: transparent draws trail the opaque ones and
/// are grid, compass and cables, none of which is an authored object.
fn picked_draws(frame: &luma_render::Frame) -> &[luma_render::frame::Draw] {
    &frame.draws[..frame.draws.len().saturating_sub(frame.transparent.len())]
}

/// CPU geometry and authored provenance for one submitted render frame.
/// It is immutable after submission and only becomes interactive when the
/// presentation carrying the same serial becomes the displayed image.
pub(super) struct PickSnapshot {
    pub(super) camera: Camera,
    pub(super) geometry: Arc<PickGeometry>,
    /// The pivot the frame drew its gizmo on, carried over from
    /// [`luma_render::Frame::gizmo_pivot`] so a press tests the widget that is
    /// actually on screen.
    pub(super) gizmo_pivot: Option<Vec3>,
    pub(super) gizmo_space: luma_scene::gizmo::GizmoSpace,
}

/// What survives between frames so the geometry above can be reused: the
/// per-asset BVHs and the last geometry built from them.
#[derive(Default)]
pub(super) struct PickCache {
    /// Immutable per-asset CPU BVHs; frame snapshots only carry Arc handles.
    meshes: HashMap<String, Arc<TriMesh>>,
    geometry: Option<Arc<PickGeometry>>,
}

impl MeshSource for PickSnapshot {
    fn mesh(&self, handle: MeshHandle) -> Option<&TriMesh> {
        self.geometry
            .meshes
            .get(handle.0 as usize)
            .map(AsRef::as_ref)
    }
}

impl PickSnapshot {
    pub(super) fn from_frame(
        frame: &luma_render::Frame,
        scene: &scene_desc::Scene,
        camera: Camera,
        cache: &mut PickCache,
    ) -> Self {
        let geometry = match cache
            .geometry
            .take()
            .filter(|resident| resident.matches(frame))
        {
            Some(resident) => resident,
            None => Arc::new(PickGeometry::build(frame, scene, &mut cache.meshes)),
        };
        cache.geometry = Some(Arc::clone(&geometry));
        Self {
            camera,
            geometry,
            gizmo_pivot: frame.gizmo_pivot,
            gizmo_space: scene.editor.gizmo_space,
        }
    }

    pub(super) fn ray(&self, at: Vec2, viewport: Vec2) -> luma_scene::Ray {
        self.camera.pixel_ray(at, viewport)
    }

    pub(super) fn pick(&self, at: Vec2, viewport: Vec2) -> Option<EditorObject> {
        self.geometry
            .graph
            .raycast(self.ray(at, viewport), Default::default(), self)
            .into_iter()
            .find_map(|hit| self.geometry.objects.get(hit.node.0 as usize)?.clone())
    }

    pub(super) fn marquee(&self, marquee: Marquee, viewport: Vec2) -> Vec<EditorObject> {
        self.geometry
            .ordered
            .iter()
            .filter(|object| {
                self.geometry
                    .anchors
                    .get(*object)
                    .is_some_and(|anchor| marquee.contains_world(&self.camera, viewport, *anchor))
            })
            .cloned()
            .collect()
    }
}

impl PickGeometry {
    fn build(
        frame: &luma_render::Frame,
        scene: &scene_desc::Scene,
        cache: &mut HashMap<String, Arc<TriMesh>>,
    ) -> Self {
        let meshes: Vec<Arc<TriMesh>> = frame
            .meshes
            .iter()
            .map(|mesh| {
                Arc::clone(cache.entry(mesh.key.clone()).or_insert_with(|| {
                    Arc::new(TriMesh::new(
                        mesh.vertices
                            .iter()
                            .map(|vertex| Vec3::from(vertex.position))
                            .collect(),
                        mesh.indices
                            .chunks_exact(3)
                            .map(|tri| [tri[0], tri[1], tri[2]])
                            .collect(),
                    ))
                }))
            })
            .collect();
        let mut graph = SceneGraph::new();
        let mut objects: Vec<Option<EditorObject>> = Vec::new();
        let mut anchors = HashMap::new();
        let mut bounds: HashMap<EditorObject, Aabb> = HashMap::new();
        let mut ordered = Vec::new();
        let mut picked = Vec::new();
        for draw in picked_draws(frame) {
            let Some(object) = draw.editor_object.clone() else {
                continue;
            };
            picked.push((draw.mesh, draw.model, object.clone()));
            let mesh_bounds = meshes[draw.mesh].bounds();
            if !mesh_bounds.is_empty() {
                let draw_bounds = Aabb::from_points(
                    mesh_bounds
                        .corners()
                        .map(|corner| draw.model.transform_point3(corner)),
                );
                bounds
                    .entry(object.clone())
                    .or_insert(Aabb::EMPTY)
                    .union(&draw_bounds);
            }
            let (scale, rotation, translation) = draw.model.to_scale_rotation_translation();
            let node = graph.insert(
                None,
                Transform {
                    translation,
                    rotation,
                    scale,
                },
                NodeContent::Mesh {
                    mesh: MeshHandle(draw.mesh as u32),
                    material: MaterialHandle(0),
                },
                NodeFlags::DEFAULT,
            );
            // First draw of an object is its anchor; every draw names it.
            if !anchors.contains_key(&object) {
                ordered.push(object.clone());
                anchors.insert(
                    object.clone(),
                    object_pose(scene, &object).map_or(translation, |pose| pose.anchor),
                );
            }
            if objects.len() <= node.0 as usize {
                objects.resize(node.0 as usize + 1, None);
            }
            objects[node.0 as usize] = Some(object);
        }
        graph.update_world_transforms();
        Self {
            graph,
            meshes,
            objects,
            ordered,
            anchors,
            bounds,
            mesh_keys: frame.meshes.iter().map(|mesh| mesh.key.clone()).collect(),
            draws: picked,
        }
    }
}

pub(super) struct SerialPairing<T> {
    pending: BTreeMap<u64, T>,
}

impl<T> Default for SerialPairing<T> {
    fn default() -> Self {
        Self {
            pending: BTreeMap::new(),
        }
    }
}

impl<T> SerialPairing<T> {
    pub(super) fn submitted(&mut self, serial: u64, snapshot: T, outcome: SubmitOutcome) {
        if let SubmitOutcome::Replaced { dropped_serial } = outcome {
            self.pending.remove(&dropped_serial);
        }
        self.pending.insert(serial, snapshot);
    }

    pub(super) fn presented(&mut self, serial: u64) -> Option<T> {
        let snapshot = self.pending.remove(&serial);
        self.pending.retain(|candidate, _| *candidate > serial);
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_snapshots_follow_presented_serial_not_latest_submission() {
        let mut pairing = SerialPairing::default();
        pairing.submitted(1, "pixels-one", SubmitOutcome::Queued);
        pairing.submitted(2, "pixels-two", SubmitOutcome::Queued);
        pairing.submitted(
            3,
            "pixels-three",
            SubmitOutcome::Replaced { dropped_serial: 2 },
        );

        assert_eq!(pairing.presented(1), Some("pixels-one"));
        assert_eq!(pairing.presented(3), Some("pixels-three"));
        assert_eq!(pairing.presented(2), None);
    }

    #[test]
    fn presenting_newer_pixels_discards_stale_pick_worlds() {
        let mut pairing = SerialPairing::default();
        pairing.submitted(7, "old", SubmitOutcome::Queued);
        pairing.submitted(8, "displayed", SubmitOutcome::Queued);
        pairing.submitted(9, "future", SubmitOutcome::Queued);

        assert_eq!(pairing.presented(8), Some("displayed"));
        assert_eq!(pairing.presented(7), None);
        assert_eq!(pairing.presented(9), Some("future"));
    }

    #[test]
    fn picking_aims_with_the_stored_camera_not_the_shaken_one() {
        let camera = Camera {
            target: Vec3::new(1.0, -2.0, 3.0),
            ..Default::default()
        };
        let mut render = scene_desc::RenderSettings::dark_stage(50.0, 0.5);
        // As much shake as the dials give.
        render.look.footage = scene_desc::Footage {
            enabled: true,
            handheld: 1.0,
            bass: 1.0,
            ..scene_desc::Footage::OFF
        };
        let scene = scene_desc::Scene {
            id: "shake".into(),
            times: vec![0.0],
            editing: true,
            aim_arrows: false,
            camera: scene_desc::CameraPose {
                position: coords::three_from_world(camera.position()).to_array(),
                target: coords::three_from_world(camera.target).to_array(),
            },
            render,
            selected_fixture_ids: Vec::new(),
            editor: Default::default(),
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
        let viewport = Vec2::new(1200.0, 800.0);
        let object = EditorObject::StagePiece("deck".into());
        let mut still = None;
        for step in 0..8 {
            let moment = luma_render::Moment {
                bass: 1.0,
                ..luma_render::Moment::at(0.3 + f64::from(step) * 0.25)
            };
            let frame = build_frame_at(
                &scene,
                &Default::default(),
                &|_, _| None,
                moment,
                &mut library,
            )
            .unwrap();
            // The picture is shaken...
            let shaken = (frame.camera.target - frame.camera.eye)
                .angle_between(camera.target - camera.position())
                .to_degrees();
            assert!(shaken > 0.01, "the frame's camera turned {shaken} degrees");
            // ...and the pick is not: the stored camera, the same ray, the
            // same object under the point it aims at.
            let pick = PickSnapshot::from_frame(&frame, &scene, camera, &mut PickCache::default());
            assert_eq!(pick.camera, camera);
            // The middle of the deck's top, which the eye looks down on.
            let bounds = pick.geometry.bounds[&object];
            let top = bounds.center().with_z(bounds.max.z - 0.02);
            let ndc = camera.project(top, viewport.x / viewport.y);
            let at = Vec2::new(
                (ndc.x + 1.0) / 2.0 * viewport.x,
                (1.0 - ndc.y) / 2.0 * viewport.y,
            );
            assert_eq!(pick.pick(at, viewport), Some(object.clone()));
            let ray = pick.ray(at, viewport);
            let ray = (ray.origin, ray.dir);
            assert_eq!(*still.get_or_insert(ray), ray);
        }
    }

    #[test]
    fn the_pick_geometry_is_reused_until_a_draw_moves() {
        let camera = Camera::default();
        let mut scene = scene_desc::Scene {
            id: "reuse".into(),
            times: vec![0.0],
            editing: true,
            aim_arrows: false,
            camera: scene_desc::CameraPose {
                position: coords::three_from_world(camera.position()).to_array(),
                target: coords::three_from_world(camera.target).to_array(),
            },
            render: scene_desc::RenderSettings::dark_stage(50.0, 0.5),
            selected_fixture_ids: Vec::new(),
            editor: Default::default(),
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
        let mut cache = PickCache::default();
        let mut snapshot = |scene: &scene_desc::Scene, cache: &mut PickCache| {
            let frame = luma_render::build_frame_with(
                scene,
                &Default::default(),
                &|_, _| None,
                0.0,
                &mut library,
            )
            .unwrap();
            PickSnapshot::from_frame(&frame, scene, camera, cache)
        };
        let first = snapshot(&scene, &mut cache);
        let again = snapshot(&scene, &mut cache);
        assert!(
            Arc::ptr_eq(&first.geometry, &again.geometry),
            "an unchanged draw list must not rebuild the hit-test geometry"
        );
        scene.pieces[0].pos[0] = 4.0;
        let moved = snapshot(&scene, &mut cache);
        assert!(
            !Arc::ptr_eq(&again.geometry, &moved.geometry),
            "a moved piece must rebuild it"
        );
        let object = EditorObject::StagePiece("deck".into());
        assert_ne!(
            again.geometry.bounds[&object].min, moved.geometry.bounds[&object].min,
            "the rebuilt bounds must follow the piece"
        );
    }
}
