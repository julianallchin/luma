//! A camera export written, read back and applied reproduces the same view.
use std::collections::BTreeMap;

use glam::{Mat4, Vec3};
use luma_render::{
    assets::{Library, Material, Vertex},
    build_frame_with,
    camera_export::{CameraExport, Context, Orbit, SceneIdentity, Viewport},
    frame::{Draw, MaterialTextures, MeshData},
    scene_desc::{CameraPose, RenderSettings, Scene, VenueEnvironment, VenueHaze},
    Frame,
};

fn frame(eye: Vec3, target: Vec3) -> Frame {
    let render = RenderSettings::room(
        VenueEnvironment::outdoor(4.0).with_sun_azimuth(135.0),
        VenueHaze::default(),
        45.0,
        1.0,
    );
    // Camera poses are three-space: (x, up, -y).
    let three = |p: Vec3| [p.x, p.z, -p.y];
    let scene = Scene {
        id: "camera-export".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: three(eye),
            target: three(target),
        },
        editing: false,
        aim_arrows: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces: Vec::new(),
        state: BTreeMap::new(),
    };
    let mut frame = build_frame_with(
        &scene,
        &BTreeMap::new(),
        &|_, _| None,
        0.0,
        &mut Library::default(),
    )
    .unwrap();
    // One caster: a tower, so the cascades reach back to something.
    let mesh = frame.meshes.len();
    let corner = |x: f32, y: f32, z: f32| Vertex {
        position: [x, y, z],
        normal: [0.0, 0.0, 1.0],
        uv: [0.0; 2],
        tangent: [1.0, 0.0, 0.0, 1.0],
    };
    frame.meshes.push(MeshData {
        key: "::camera-export-tower".into(),
        vertices: vec![corner(-0.5, -0.5, 0.0), corner(0.5, 0.5, 1.0)].into(),
        indices: vec![0, 1, 0].into(),
    });
    let at = frame.draws.len() - frame.transparent.len();
    frame.draws.insert(
        at,
        Draw {
            mesh,
            model: Mat4::from_translation(Vec3::new(2.0, 1.0, 0.0))
                * Mat4::from_scale(Vec3::new(0.4, 0.4, 9.0)),
            material: Material::default(),
            textures: MaterialTextures::default(),
            editor_object: None,
        },
    );
    frame
}

fn context() -> Context {
    Context {
        exported_at: "20260924T120000Z".into(),
        scene: SceneIdentity {
            venue_id: "venue".into(),
            venue_name: "Venue".into(),
            score_id: Some("score".into()),
            playhead_s: 12.5,
        },
        viewport: Viewport {
            width: 1280,
            height: 720,
            scale_factor: 2.0,
        },
        orbit: Some(Orbit {
            target: [0.0, 0.0, 1.0],
            radius: 31.6,
            azimuth: -1.57,
            polar: 1.25,
        }),
    }
}

#[test]
fn an_exported_view_reads_back_and_reapplies_to_the_same_matrices() {
    let exported = CameraExport::capture(
        &frame(Vec3::new(3.0, -30.0, 10.0), Vec3::new(0.0, 0.0, 1.0)),
        context(),
    );
    assert_eq!(
        exported.shadow.cascades.len(),
        3,
        "an outdoor room has a sun"
    );
    assert!(!exported.casters.is_empty());

    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("camera-export.json");
    exported.write(&path).unwrap();
    let read = CameraExport::read(&path).unwrap();
    assert_eq!(read, exported, "the JSON round trip changed a value");

    // A frame from somewhere else, put back at the exported view.
    let mut elsewhere = frame(Vec3::new(-40.0, 5.0, 3.0), Vec3::ZERO);
    read.apply(&mut elsewhere);
    let again = CameraExport::capture(&elsewhere, context());
    assert_eq!(again.camera, exported.camera);
    assert_eq!(again.sun, exported.sun);
    assert_eq!(again.shadow, exported.shadow);
    assert_eq!(read.view(), again.view());
    assert_eq!(read.projection(), again.projection());
    assert_eq!(read.cascade_matrices(), again.cascade_matrices());
}
