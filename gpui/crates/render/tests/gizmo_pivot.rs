//! Where the transform gizmo stands, in numbers.
//!
//! `Frame::gizmo_pivot` is the one point the widget is drawn on and the one
//! point the editor picks against, so it is worth pinning exactly rather than
//! inferring from pixels. Two rules, both of which used to be somewhere else or
//! nowhere:
//!
//! * a **fixture** anchors on its own origin — in the renderer's world space,
//!   which is the data-space triple mirrored in Y (`coords::world_from_data`);
//! * a **stage piece** anchors on the bottom centre of its mesh's bounds,
//!   because stage GLBs put their local origin at a corner and a widget on that
//!   origin floats off the piece entirely.
//!
//! The second is `unified-transform.tsx::stagePieceAnchorWorld`, ported. Its
//! third case — a parented piece anchoring on the socket that attaches it —
//! cannot come across yet: a `Piece` arrives with its parent chain already
//! flattened away and the socket catalogue is still TypeScript-only. See
//! `docs/design/venue-graph.md`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use glam::{Mat4, Vec3};
use luma_render::assets::Library;
use luma_render::build_frame_with;
use luma_render::coords;
use luma_render::scene_desc::{
    CameraPose, Editor, Fixture, Geometry, Piece, RenderSettings, Scene,
};

/// A piece whose local origin is nowhere near the centre of its footprint.
const DECK: &str = "stage_lab/stage_praticavel_2x1x1.glb";

/// The fixture's stored pose, off the `y = 0` plane — the only plane the
/// data→world mirror leaves fixed, and so the only place a test could not tell
/// the two spaces apart.
const MOVER_POS: [f32; 3] = [1.5, 2.0, 3.0];

/// The deck's stored pose: turned, and away from the origin, so a transform
/// composed in the wrong space lands somewhere else entirely.
const DECK_POS: [f32; 3] = [-2.0, 4.0, 0.0];
const DECK_ROT: [f32; 3] = [0.0, 0.0, 0.7];

fn library() -> Library {
    Library::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"))
}

fn scene() -> Scene {
    Scene {
        id: "gizmo-pivot".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [4.5, 3.0, 5.0],
            target: [0.0, 0.8, 0.0],
        },
        editing: true,
        aim_arrows: false,
        render: RenderSettings::dark_stage(50.0, 0.5),
        selected_fixture_ids: Vec::new(),
        editor: Editor::default(),
        fixtures: vec![Fixture {
            id: "mover".into(),
            fixture_path: "Luma/Mover.qxf".into(),
            mode_name: "Default".into(),
            pos: MOVER_POS,
            rot: [0.0; 3],
        }],
        pieces: vec![Piece {
            id: "deck".into(),
            geometry: Geometry::mesh(DECK),
            kind: "floor".into(),
            pos: DECK_POS,
            rot: DECK_ROT,
            scale: 1.0,
        }],
        state: BTreeMap::new(),
    }
}

/// The pivot the renderer would draw the widget on for one selection.
fn pivot_of(fixtures: &[&str], pieces: &[&str]) -> Option<Vec3> {
    let mut scene = scene();
    scene.selected_fixture_ids = fixtures.iter().map(|id| (*id).to_string()).collect();
    scene.editor.selected_piece_ids = pieces.iter().map(|id| (*id).to_string()).collect();
    scene.editor.gizmo_piece_ids = scene.editor.selected_piece_ids.clone();
    build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library())
        .expect("the frame should build")
        .gizmo_pivot
}

#[test]
fn a_fixture_anchors_on_its_own_origin_in_world_space() {
    let pivot = pivot_of(&["mover"], &[]).expect("a selected fixture has a pivot");
    let expected = coords::world_from_data(Vec3::from(MOVER_POS));
    assert!(
        pivot.abs_diff_eq(expected, 1e-4),
        "the widget stands at {pivot:?} rather than on the fixture at {expected:?} — \
         a stored triple is not a world pose, and the difference is a mirror in Y"
    );
}

#[test]
fn a_piece_anchors_on_the_bottom_centre_of_its_footprint() {
    let pivot = pivot_of(&[], &["deck"]).expect("a selected piece has a pivot");

    // The rule, spelled out: bottom centre of the mesh's own bounds, carried
    // through the pose the piece is drawn at.
    let (lo, hi) = library().get(DECK).expect("the deck mesh loads").bounds();
    let expected = (Mat4::from_mat3(coords::three_to_world_basis())
        * coords::three_pose_from_data(DECK_POS, DECK_ROT))
    .transform_point3(Vec3::new((lo.x + hi.x) / 2.0, lo.y, (lo.z + hi.z) / 2.0));
    assert!(
        pivot.abs_diff_eq(expected, 1e-4),
        "the widget stands at {pivot:?}, not on the deck's footprint at {expected:?}"
    );
    assert!(
        pivot.distance(coords::world_from_data(Vec3::from(DECK_POS))) > 0.3,
        "the deck's origin and its footprint centre came out the same point, so this \
         mesh cannot tell the two apart — pick another"
    );
}

#[test]
fn a_mixed_selection_anchors_between_its_members() {
    let (fixture, piece) = (
        pivot_of(&["mover"], &[]).unwrap(),
        pivot_of(&[], &["deck"]).unwrap(),
    );
    let both = pivot_of(&["mover"], &["deck"]).expect("a mixed selection has a pivot");
    assert!(both.abs_diff_eq((fixture + piece) / 2.0, 1e-4));
}

#[test]
fn nothing_selected_draws_no_widget() {
    assert_eq!(pivot_of(&[], &[]), None);
}

#[test]
fn painted_axes_and_pick_axes_share_uvz_including_a_tilted_mount() {
    use luma_scene::{Axis, GizmoHandle, GizmoMode, Ray};
    for basis in [glam::Quat::IDENTITY, glam::Quat::from_rotation_x(0.55)] {
        let mut scene = scene();
        scene.selected_fixture_ids = vec!["mover".into()];
        scene.editor.gizmo_space.basis = basis;
        let frame =
            build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library()).unwrap();
        let pivot = frame.gizmo_pivot.unwrap();
        let eye = coords::world_from_three(Vec3::from(scene.camera.position));
        let view = eye - pivot;
        let scale = luma_scene::gizmo_scale(view.length(), 50.0);
        for overlay in &frame.overlays {
            if frame.meshes[overlay.mesh].key != "::gizmo-segment" {
                continue;
            }
            let axis = if overlay.color == Vec3::X {
                Axis::X
            } else if overlay.color == Vec3::Y {
                Axis::Y
            } else if overlay.color == Vec3::Z {
                Axis::Z
            } else {
                continue;
            };
            let point = overlay.model.transform_point3(Vec3::new(0.7, 0.0, 0.0));
            let direction = (point - pivot).normalize();
            assert!(direction.dot(basis * axis.vector()).abs() > 0.999);
            let hit = scene
                .editor
                .gizmo_space
                .hit(
                    Ray::new(eye, point - eye),
                    pivot,
                    scale,
                    view,
                    GizmoMode::Translate,
                )
                .unwrap();
            assert_eq!(hit.handle, GizmoHandle::TranslateAxis(axis));
        }
    }
}

#[test]
fn mounted_piece_draws_only_the_normal_rotation_ring() {
    let mut scene = scene();
    scene.selected_fixture_ids = vec!["mover".into()];
    scene.editor.gizmo = luma_scene::GizmoMode::Rotate;
    scene.editor.gizmo_space.rotation = [false, false, true];
    scene.editor.gizmo_space.basis = glam::Quat::from_rotation_x(0.55);
    let frame =
        build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library()).unwrap();
    let rings: Vec<_> = frame
        .overlays
        .iter()
        .filter(|overlay| frame.meshes[overlay.mesh].key == "::gizmo-ring")
        .collect();
    assert_eq!(rings.len(), 1);
    let normal = rings[0].model.transform_vector3(Vec3::Z).normalize();
    assert!(normal.abs_diff_eq(scene.editor.gizmo_space.basis * Vec3::Z, 1e-5));
    assert_eq!(rings[0].color, Vec3::Z);
}

#[test]
fn plane_handles_have_front_faces_in_every_camera_octant() {
    for x in [-5.0, 5.0] {
        for y in [-5.0, 5.0] {
            for z in [-5.0, 5.0] {
                let mut scene = scene();
                scene.selected_fixture_ids = vec!["mover".into()];
                let pivot = coords::three_from_data(Vec3::from(MOVER_POS));
                scene.camera.position = (pivot + Vec3::new(x, y, z)).to_array();
                let frame =
                    build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library())
                        .unwrap();
                let eye = coords::world_from_three(Vec3::from(scene.camera.position));
                let mut count = 0;
                for overlay in &frame.overlays {
                    let mesh = &frame.meshes[overlay.mesh];
                    if mesh.key != "::gizmo-quad" {
                        continue;
                    }
                    count += 1;
                    let facing = mesh
                        .indices
                        .chunks_exact(3)
                        .filter(|triangle| {
                            let point = |i: u32| {
                                overlay.model.transform_point3(Vec3::from(
                                    mesh.vertices[i as usize].position,
                                ))
                            };
                            let a = point(triangle[0]);
                            (point(triangle[1]) - a)
                                .cross(point(triangle[2]) - a)
                                .dot(eye - a)
                                > 0.0
                        })
                        .count();
                    assert_eq!(facing, 2, "plane fill vanished at {x}, {y}, {z}");
                }
                assert_eq!(count, 3);
            }
        }
    }
}

#[test]
#[ignore = "manual GPU capture for selection styling"]
fn capture_selection_brackets() {
    let mut scene = scene();
    scene.fixtures.clear();
    scene.render.environment = luma_render::scene_desc::Environment::EDITOR;
    scene.render.sun = Some(luma_render::scene_desc::DirectionalLight::EDITOR);
    scene.render.haze.enabled = false;
    scene.pieces[0].pos = [0.0, 0.0, 0.0];
    scene.pieces[0].rot = [0.0; 3];
    scene.editor.selected_piece_ids = vec![scene.pieces[0].id.clone()];
    scene.camera.position = [-3.5, 3.0, 4.5];
    scene.camera.target = [0.0, 0.5, 0.0];
    let frame =
        build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library()).unwrap();
    let pixels = luma_render::Renderer::new()
        .unwrap()
        .render(&frame, 960, 640, 4)
        .unwrap();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../harness/shots/gpui/selection-brackets.png");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::save_buffer(&path, &pixels, 960, 640, image::ColorType::Rgba8).unwrap();
    eprintln!("{}", path.display());
}
