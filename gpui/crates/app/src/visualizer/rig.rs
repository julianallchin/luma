//! Opening a stage on a venue: loading its rig and composite, relighting it
//! with another score, and describing the room to the renderer.
use super::*;

/// The builder's catalog: the same supply the backend solves with, fixture
/// bundle included. A builder that could not measure a housing would preview
/// every light half-buried in the truss it is being dropped on.
fn load_sockets() -> Option<luma_scene::catalog::VenueSockets> {
    let fixtures_root = crate::library::fixtures_root().ok();
    luma_scene::catalog::VenueSockets::load(
        luma_lib::resources::meshes_root(fixtures_root.as_deref()),
        std::sync::Arc::new(luma_lib::venue_graph::BundledFixtures(
            fixtures_root.unwrap_or_default(),
        )),
    )
    .ok()
}

impl Visualizer {
    /// Open the view on a venue and start its load.
    ///
    /// `subject` is the score that should light the rig, when the view was
    /// opened over a track editor. Compositing it is a dispatch command like
    /// any other; what is *not* a command is the per-frame sample that follows
    /// — see [`Library::sample_universe`].
    pub(crate) fn open(
        library: &Library,
        venue_id: &str,
        venue_name: String,
        subject: Option<Lit>,
        cx: &mut Context<Luma>,
    ) -> Self {
        let rig = library.venue_rig(venue_id);
        // The three local, device-global view settings — grid, gizmos and the
        // render percent. Read here rather than per venue because that is what
        // device-global means: they are the operator's answer, and the room the
        // view happens to open on is not asked.
        let settings = library.settings();
        // The venue's own environment arrives with its rig; until then the
        // default room, which is the picture this app has always opened with.
        let environment = VenueEnvironment::default();
        let composite = subject
            .clone()
            .map(|lit| (library.composite_score(&lit.score), lit));
        let venue = venue_id.to_string();
        // The builder's catalog reads every mesh it measures from disk, so it
        // loads off the UI thread while the rig does.
        let sockets = cx.background_spawn(async { load_sockets() });
        cx.spawn(async move |this, cx| {
            // The composite first: it is what makes the sample non-empty, and
            // a rig that appeared before its light would flash dark.
            let mut installed = None;
            if let Some((composite, lit)) = composite {
                if composite.await.is_ok() {
                    installed = Some(lit);
                }
            }
            let loaded = rig.await;
            let sockets = sockets.await;
            // Defaulted rather than surfaced: a settings read that failed still
            // has to leave a drawable viewport, and `AppSettings::default` is
            // exactly the chrome this app has always opened with.
            let settings = settings.await.unwrap_or_default();
            let warm = this
                .update(cx, |this, cx| {
                    // Addressed to the room, not merely to whatever stage is up: a
                    // rig landing after the eye moved to another venue must not
                    // paint into the stage that replaced it.
                    let Some(state) = this.visualizer.as_mut().filter(|it| it.venue_id == venue)
                    else {
                        return None;
                    };
                    state.composite_landed(installed);
                    state.rig_loaded(loaded, sockets);
                    state.render_controls.apply_settings(&settings);
                    cx.notify();
                    state.warm_assets()
                })
                .ok()
                .flatten();
            let Some(warm) = warm else {
                return;
            };
            let warmed = cx.background_spawn(warm).await;
            this.update(cx, |this, cx| {
                let Some(state) = this.visualizer.as_mut().filter(|it| it.venue_id == venue) else {
                    return;
                };
                let mut stage = state.stage.borrow_mut();
                if matches!(stage.assets, Some(Warming::Loading)) {
                    stage.assets = Some(Warming::Ready(warmed));
                    drop(stage);
                    this.notify_stage(cx);
                }
            })
            .ok();
        })
        .detach();

        Self {
            venue_id: venue_id.to_string(),
            venue_name,
            subject,
            lit: None,
            audition: None,
            bass: None,
            motors: Default::default(),
            gpu_enabled: stage_gpu_enabled(),
            status: Status::Loading,
            camera: opening_camera(
                &Framing::default(),
                &Viewfinder::new(FOV_Y_DEG, DEFAULT_ASPECT),
            ),
            framing: Framing::default(),
            owes_opening_pose: false,
            saved_pose: None,
            posed: false,
            drag: None,
            fly: None,
            tour: None,
            pointer_locked: false,
            fly_speed: 1.0,
            pivot_dot: None,
            editor_drag: None,
            selection: Selection::default(),
            gizmo_mode: GizmoMode::Translate,
            gizmo_hover: None,
            size: gpui::Size::default(),
            viewport_origin: Point::default(),
            venue_environment: VenueEnvironment::default(),
            render_controls: RenderControls::new(environment),
            settings: Default::default(),
            settings_page: Default::default(),
            presentation: false,
            bottom_bar_visible: false,
            settings_motion: RefCell::new(settings::DockMotion::new(cx.reduce_motion())),
            environment_saving: false,
            environment_edited: false,
            environment_pending: Rc::default(),
            haze_saving: false,
            haze_edited: false,
            haze_pending: Rc::default(),
            view_setting_saving: false,
            view_setting_pending: Rc::default(),
            fps_panel: Default::default(),
            reduced_motion: cx.reduce_motion(),
            exporting: false,
            build: None,
            rigging: false,
            work_light: luma_scene::environment::WORK_LIGHT,
            stage: Rc::default(),
        }
    }

    /// The environment edited by View and used by every preview in this venue.
    pub(crate) fn venue_environment(&self) -> VenueEnvironment {
        self.venue_environment
    }

    /// Adopt an edited environment without waiting for the write to land.
    ///
    /// The render controls are re-derived from it, which is what puts the new light on
    /// screen: its `house` field rides in the idle key, so a resting viewport
    /// wakes for the change rather than re-presenting the room it had.
    pub(crate) fn set_venue_environment(&mut self, environment: VenueEnvironment) {
        self.venue_environment = environment;
        self.render_controls.set_environment(self.environment());
    }

    /// The room as it is lit on screen. Previewing a score uses the saved
    /// room: selecting one changes its fixtures' output, not the time of day
    /// or the house lights. Rigging uses the same room under a work light of
    /// its own, which is never saved, and the camera walkthrough keeps a
    /// dark room lit enough to look around — see [`Self::tour_lit`].
    pub(crate) fn environment(&self) -> VenueEnvironment {
        if self.rigging {
            self.venue_environment.lit_at(self.work_light)
        } else {
            self.tour_lit(self.venue_environment)
        }
    }

    /// Whether the page under the stage is the one the room is built on.
    /// Arriving there turns the work light back to where it starts.
    pub(super) fn set_rigging(&mut self, rigging: bool) {
        self.rigging = rigging;
        self.set_work_light(luma_scene::environment::WORK_LIGHT);
    }

    /// Turn the work light the room is rigged under.
    pub(crate) fn set_work_light(&mut self, fraction: f32) {
        self.work_light = fraction;
        self.render_controls.set_environment(self.environment());
    }

    /// Light this stage with a different score, without rebuilding it.
    ///
    /// Moving between scores inside one room changes only *what is composited*
    /// — the rig, the camera framing and the GPU are all still about the same
    /// venue. Tearing the stage down to re-light it would drop the device and
    /// re-frame the camera, so a glance between two documents would restart the
    /// view. Compositing is a dispatch command like any other; the per-frame
    /// sample that follows is not (see [`Library::sample_universe`]).
    pub(super) fn relight(
        &mut self,
        library: &Library,
        subject: Option<Lit>,
        cx: &mut Context<Luma>,
    ) {
        self.subject = subject.clone();
        self.render_controls.set_environment(self.environment());
        let Some(lit) = subject else {
            // Nothing on screen is about a score any more, so the stage stops
            // claiming one. What the engine still holds is stale and unread —
            // the render controls have already gone back to editor lighting.
            self.lit = None;
            return;
        };
        let composite = library.composite_score(&lit.score);
        cx.spawn(async move |this, cx| {
            let landed = composite.await.is_ok().then_some(lit);
            this.update(cx, |this, cx| {
                if let Some(state) = this.visualizer.as_mut() {
                    state.composite_landed(landed);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Adopt an install that has come back, unless the eye moved on while it
    /// was in flight.
    ///
    /// Two composites can be outstanding at once — a fast one issued after a
    /// slow one — and the later *answer* is not necessarily the later *ask*.
    /// Checking against the current subject is what keeps the readout from
    /// naming a score the stage has already left.
    fn composite_landed(&mut self, landed: Option<Lit>) {
        if landed.is_some() && landed != self.subject {
            return;
        }
        if let Some(landed) = landed {
            self.lit = Some(landed);
        }
    }

    /// Read and decode everything the rig's first frame draws — meshes,
    /// floor, sky — on a worker, by building that frame there once. The
    /// renderer waits for it; a renderer already running has its own library.
    fn warm_assets(
        &mut self,
    ) -> Option<impl std::future::Future<Output = assets::Library> + use<>> {
        let mut stage = self.stage.borrow_mut();
        if !self.gpu_enabled || stage.gpu.is_some() || stage.assets.is_some() {
            return None;
        }
        let scene = stage.scene.clone()?;
        let definitions = stage.definitions.clone();
        stage.assets = Some(Warming::Loading);
        Some(async move {
            let mut assets = assets::Library::new(luma_lib::resources::meshes_root(None));
            // A frame that fails here fails the same way on the stage, which
            // says so; this one only loads.
            let _ = luma_render::build_frame(&scene, &definitions, 0.0, &mut assets);
            assets
        })
    }

    /// A rig re-read after a graph edit. The camera stays where the operator
    /// left it — a builder that re-framed on every placement would throw the
    /// view away once per piece.
    pub(crate) fn rig_reloaded(&mut self, loaded: Result<Rig, LibraryError>) {
        let framing = self.framing.clone();
        let camera = self.camera;
        self.rig_loaded(loaded, None);
        self.framing = framing;
        self.camera = camera;
        self.owes_opening_pose = false;
    }

    /// Open on the pose this venue was left in, or framed on the rig when
    /// there is none. The lens is the framing's either way.
    pub(super) fn opening_pose(&self) -> Camera {
        let camera = opening_camera(&self.framing, &self.view_finder());
        match self.saved_pose {
            Some(pose) => pose.applied(camera),
            None => camera,
        }
    }

    /// The pose to remember, once there is a real one.
    pub(crate) fn settled_pose(&self) -> Option<Pose> {
        self.posed.then(|| Pose::of(&self.camera))
    }

    /// Hand the stage the pose its venue was last left in.
    pub(crate) fn restore_pose(&mut self, pose: Option<Pose>) {
        self.saved_pose = pose;
    }

    /// `sockets` is the builder's catalog, when it was loaded ahead; without
    /// it, a stage with no builder yet loads one here.
    fn rig_loaded(
        &mut self,
        loaded: Result<Rig, LibraryError>,
        sockets: Option<luma_scene::catalog::VenueSockets>,
    ) {
        let rig = match loaded {
            Ok(rig) => rig,
            Err(error) => {
                log::warn!("could not load the rig: {error}");
                self.status = Status::Empty("Could not load the rig".to_string());
                return;
            }
        };
        // The builder first, and whether or not there is anything to draw: an
        // empty room is exactly the room a builder is for, and a page that
        // waited for a piece before offering a palette could never place the
        // first one.
        match self.build.as_mut() {
            Some(build) => build.adopt(&rig),
            None => {
                self.build = sockets
                    .or_else(load_sockets)
                    .and_then(|sockets| crate::stage::Build::new(&self.venue_id, &rig, sockets));
            }
        }
        // No early return for an empty rig. A venue with nothing in it is
        // exactly the venue the builder exists to fill, and returning here left
        // it with no scene — so no floor, no grid, and nothing to put a first
        // piece *on*. The room is built whether or not anything is patched;
        // an empty rig is no reason to draw an empty pane.
        let definitions: BTreeMap<_, _> = rig
            .definitions
            .iter()
            .map(|(path, def)| (path.clone(), luma_scene::definition::Definition::from(def)))
            .collect();
        if !self.environment_edited {
            self.venue_environment = rig.environment;
        }
        // Its own guard, not the environment's: a density scrub in flight is a
        // local edit the rig read knows nothing about, and adopting the stored
        // row over it would snap the slider back under the hand.
        if !self.haze_edited {
            self.render_controls.haze = rig.haze;
        }
        self.render_controls.set_environment(self.environment());
        let graph = rig.rows.to_graph();
        let room = graph
            .as_ref()
            .and_then(|graph| graph.node(graph.root()))
            .map(|root| &root.params);
        self.render_controls.room = room
            .and_then(luma_scene::venue::room_size)
            .map(|size| size.map(|m| m as f32));
        self.render_controls.wall_shade =
            room.map_or(luma_scene::venue::WALL_SHADE, luma_scene::venue::wall_shade);
        let scene = scene(
            &rig,
            &definitions,
            self.environment(),
            self.render_controls.haze,
        );
        self.framing = scene.framing(&definitions);
        self.camera = self.opening_pose();
        self.owes_opening_pose = true;
        // A selection names authored objects; a reloaded scene may no longer
        // have some of them. Dropping the dead names here is what makes a
        // deleted piece *deselected* rather than a slot for the next pick to
        // misresolve.
        self.selection.retain(|object| match object {
            EditorObject::Fixture(id) => scene.fixtures.iter().any(|f| f.id.as_str() == id),
            EditorObject::StagePiece(id) => scene.pieces.iter().any(|p| p.id.as_str() == id),
        });
        let mut stage = self.stage.borrow_mut();
        stage.definitions = definitions;
        stage.lights = luma_lib::stage_render::StageLights::new(&rig.fixtures, &rig.definitions);
        stage.strobes = FixtureStrobes::new(&rig);
        stage.lasers = rig.lasers.clone();
        stage.scene = Some(scene);
        // A new scene is a frame owed. The idle gate's key deliberately does
        // not carry scene contents, so a resting viewport handed a reloaded
        // rig would keep re-presenting the room it had — a deleted piece
        // stayed on screen until a click or the camera changed the key.
        stage.idle = None;
        drop(stage);
        self.status = Status::Live;
    }

    /// Start the renderer worker once, on the first frame with something to draw.
    ///
    /// GPU acquisition itself happens on that worker. A failure is returned as
    /// an asynchronous frame result, so opening the screen never blocks the UI.
    pub(super) fn gpu_ready(&mut self) -> bool {
        if !self.gpu_enabled {
            return false;
        }
        if self.stage.borrow().gpu.is_some() {
            return true;
        }
        let mut stage = self.stage.borrow_mut();
        let assets = match stage.assets.take() {
            Some(Warming::Ready(assets)) => assets,
            _ => assets::Library::new(luma_lib::resources::meshes_root(None)),
        };
        stage.gpu = Some(Gpu {
            viewport: AsyncViewport::new(),
            work: StageWork::default(),
            submission: Submission::default(),
            assets,
            picks: PickTimeline::default(),
            pick_cache: PickCache::default(),
            exported: None,
        });
        true
    }
}

// -- the venue, in the renderer's vocabulary ---------------------------------

/// One venue as a scene description: geometry in data space, the dark-stage
/// render dials, and an **empty** state map — head state
/// arrives per frame through [`luma_render::StateSource`] instead.
///
/// Every pose here is read off the solved graph, through the same
/// [`stage_render::piece_of`] the offscreen path uses: the two differ in
/// render settings, never in where anything is.
pub(crate) fn scene(
    rig: &Rig,
    definitions: &BTreeMap<String, luma_scene::definition::Definition>,
    environment: VenueEnvironment,
    haze: luma_scene::environment::VenueHaze,
) -> scene_desc::Scene {
    // A fixture node's id *is* its `fixtures` row id, which is what makes the
    // patch and the placement two halves of one fixture without either half
    // storing the other's key.
    let placed: HashMap<&str, &_> = rig
        .venue
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    scene_desc::Scene {
        id: "live".into(),
        times: Vec::new(),
        camera: scene_desc::CameraPose {
            position: [0.0; 3],
            target: [0.0; 3],
        },
        editing: true,
        aim_arrows: false,
        // The one preset a venue is drawn under, with the haze resolution
        // reduced for the live path. The render controls overwrite the fill dials each
        // frame; the house, the sky and the haze are re-read from this
        // environment and this haze there too, so the lamps hang and the room
        // has its atmosphere from the first frame on.
        render: scene_desc::RenderSettings::room(
            environment,
            haze,
            FOV_Y_DEG,
            luma_render::LIVE_HAZE_RESOLUTION,
        ),
        selected_fixture_ids: Vec::new(),
        editor: scene_desc::Editor::default(),
        // A fixture whose definition did not resolve has no mesh and no cone,
        // and one nobody has placed has nowhere to be, so both are left out
        // rather than drawn as a guess at the origin.
        fixtures: rig
            .fixtures
            .iter()
            .filter(|f| definitions.contains_key(&f.fixture_path))
            .filter_map(|f| {
                let node = placed.get(f.id.as_str())?;
                Some(scene_desc::Fixture {
                    id: f.id.clone(),
                    fixture_path: f.fixture_path.clone(),
                    mode_name: f.mode_name.clone(),
                    pos: node.position.map(|v| v as f32),
                    rot: node.rotation.map(|v| v as f32),
                    aim: luma_scene::venue::ANGLE_PARAMS
                        .map(|key| node.params.get(key).copied().unwrap_or(0.0) as f32),
                })
            })
            .collect(),
        pieces: rig
            .venue
            .nodes
            .iter()
            .filter_map(stage_render::piece_of)
            .collect(),
        state: BTreeMap::new(),
    }
}
