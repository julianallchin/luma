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
