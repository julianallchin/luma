//! Fixtures shared by the visualizer's unit tests: a stage with no renderer
//! and an empty hit-test snapshot.
use super::*;

/// A snapshot with nothing in it: the camera is all these tests read.
pub(super) fn empty_pick(camera: Camera) -> PickSnapshot {
    PickSnapshot {
        camera,
        geometry: Arc::new(PickGeometry {
            graph: SceneGraph::new(),
            meshes: Vec::new(),
            objects: Vec::new(),
            ordered: Vec::new(),
            anchors: HashMap::new(),
            bounds: HashMap::new(),
            mesh_keys: Vec::new(),
            draws: Vec::new(),
        }),
        gizmo_pivot: None,
        gizmo_space: Default::default(),
    }
}

pub(super) fn visualizer(build: Option<crate::stage::Build>, pick: PickSnapshot) -> Visualizer {
    let camera = pick.camera;
    Visualizer {
        venue_id: "venue".into(),
        venue_name: "Venue".into(),
        subject: None,
        lit: None,
        audition: None,
        bass: None,
        motors: Default::default(),
        gpu_enabled: false,
        status: Status::Loading,
        camera,
        framing: Default::default(),
        owes_opening_pose: false,
        saved_pose: None,
        posed: false,
        drag: None,
        fly: None,
        pointer_locked: false,
        fly_speed: 1.0,
        pivot_dot: None,
        editor_drag: None,
        selection: Default::default(),
        gizmo_mode: Default::default(),
        gizmo_hover: None,
        size: gpui::size(px(1200.0), px(800.0)),
        viewport_origin: Default::default(),
        venue_environment: Default::default(),
        render_controls: RenderControls::new(Default::default()),
        settings: Default::default(),
        settings_page: Default::default(),
        presentation: false,
        bottom_bar_visible: false,
        settings_motion: RefCell::new(settings::DockMotion::new(true)),
        selection_motion: SelectionMotion::new(true),
        selection_card_side: None,
        selection_card_last: None,
        environment_saving: false,
        environment_edited: false,
        environment_pending: Rc::default(),
        haze_saving: false,
        haze_edited: false,
        haze_pending: Rc::default(),
        view_setting_saving: false,
        view_setting_pending: Rc::default(),
        fps_panel: Default::default(),
        reduced_motion: true,
        exporting: false,
        build,
        stage: Rc::new(RefCell::new(Stage {
            displayed_pick: Some(pick),
            ..Default::default()
        })),
    }
}
