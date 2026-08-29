//! Where the renderer puts a beam today, pinned — and the same numbers
//! `fixture-kinematics` produces, checked on this workspace's toolchain.
//!
//! Two things live here, and they are deliberately separate:
//!
//! 1. **Characterization.** `frame::build` currently sites a moving head's cone
//!    at `base.transform_point3(Vec3::ZERO)` — the bare mounting origin, with no
//!    pivot and no aperture depth. That is almost certainly wrong (a beam should
//!    leave the lens, not the clamp), but it is what every committed golden was
//!    captured against, so it is pinned here *before* anything moves. When the
//!    renderer is swapped onto `fixture_kinematics::beam_ray`, this test is the
//!    diff: it should fail in exactly the way the new geometry predicts, and its
//!    expectations are updated in the same commit that recaptures the goldens.
//!
//! 2. **Agreement.** The shared contract vectors, evaluated here rather than in
//!    `src-tauri/`. The app and the renderer are separate cargo workspaces and
//!    cannot share a test crate, so the file is included by path from both.

use std::collections::BTreeMap;
use std::path::PathBuf;

use glam::{Mat3, Mat4, Vec3};
use luma_render::assets::Library;
use luma_render::coords::{euler_xyz, three_to_world_basis, world_from_data};
use luma_render::scene_desc::{
    CameraPose, Definition, Dimensions, Fixture, Lens, Mode, Physical, PrimitiveState,
    RenderSettings, Scene,
};
use luma_render::{build_frame, Frame};

#[path = "../../../../src-tauri/crates/fixture-kinematics/contract_vectors.rs"]
mod contract_vectors;

/// The pose the characterization frame uses: nothing symmetric, so a dropped
/// term or a flipped sign cannot cancel out.
const MOUNT_POSITION: [f32; 3] = [-1.5, 2.75, 5.25];
const MOUNT_ROTATION: [f32; 3] = [0.31, -0.62, 0.94];
const PAN_DEG: f32 = 52.0;
const TILT_DEG: f32 = -19.0;

fn mover_definition() -> Definition {
    Definition {
        kind: "Moving Head".into(),
        modes: vec![Mode {
            name: "Standard".into(),
            heads: Vec::new(),
        }],
        physical: Some(Physical {
            dimensions: Some(Dimensions {
                width: 300.0,
                height: 420.0,
                depth: 300.0,
            }),
            layout: None,
            lens: Some(Lens {
                degrees_min: 14.0,
                degrees_max: 14.0,
            }),
        }),
    }
}

fn characterization_frame() -> Frame {
    let mut state = BTreeMap::new();
    state.insert(
        "mover:0".to_string(),
        PrimitiveState {
            dimmer: 1.0,
            color: [1.0, 1.0, 1.0],
            strobe: 0.0,
            position: [PAN_DEG, TILT_DEG],
            gobo: 0,
            gobo_rotation: 0.0,
        },
    );
    let scene = Scene {
        id: "fixture-kinematics-characterization".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [5.0, 3.0, 5.0],
            target: [0.0, 1.5, 0.0],
        },
        editing: false,
        render: RenderSettings::dark_stage(48.0, 0.5),
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: vec![Fixture {
            id: "mover".into(),
            fixture_path: "mover.qxf".into(),
            mode_name: "Standard".into(),
            pos: MOUNT_POSITION,
            rot: MOUNT_ROTATION,
        }],
        pieces: Vec::new(),
        state,
    };
    let mut definitions = BTreeMap::new();
    definitions.insert("mover.qxf".to_string(), mover_definition());

    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    build_frame(&scene, &definitions, 0.0, &mut Library::new(meshes))
        .expect("characterization scene should build")
}

#[test]
fn beam_origin_today_is_the_bare_mounting_origin() {
    let frame = characterization_frame();
    let cone = frame
        .fixture_cones
        .first()
        .expect("a lit moving head should emit one cone");

    // The pinned value, spelled as the renderer derives it: the mounting origin
    // and nothing else. Pivot offset and aperture depth do not appear, which is
    // the bug this pinning exists to make visible.
    let to_world = Mat4::from_mat3(three_to_world_basis());
    let pos_three = Vec3::new(MOUNT_POSITION[0], MOUNT_POSITION[2], MOUNT_POSITION[1]);
    let rot_three = euler_xyz(MOUNT_ROTATION[0], MOUNT_ROTATION[2], MOUNT_ROTATION[1]);
    let base = to_world * Mat4::from_translation(pos_three) * Mat4::from_mat3(rot_three);
    let expected = base.transform_point3(Vec3::ZERO);

    assert!(
        cone.position.abs_diff_eq(expected, 1e-6),
        "beam origin moved: {:?} vs the pinned {expected:?}",
        cone.position
    );
    // And, literally, so that a change to the derivation above cannot silently
    // agree with itself.
    assert!(
        cone.position
            .abs_diff_eq(Vec3::new(-1.5, -2.75, 5.25), 1e-5),
        "beam origin moved: {:?}",
        cone.position
    );
}

#[test]
fn kinematics_reproduces_todays_beam_direction() {
    // Direction is the half of the ray the renderer already gets right, so the
    // crate must agree with it *now*, unconditionally. Only the origin is
    // expected to change when the swap lands.
    let frame = characterization_frame();
    let cone = frame.fixture_cones.first().expect("one cone");

    let geom = fixture_kinematics::FixtureGeometry::unauthored(vec![Vec3::ZERO]);
    let mount = fixture_kinematics::Mount::from_stored(Vec3::from(MOUNT_POSITION), MOUNT_ROTATION);
    let art = fixture_kinematics::Articulation::from_degrees(PAN_DEG, TILT_DEG);
    let ray = fixture_kinematics::beam_ray(&geom, &mount, &art, 0);

    assert!(
        world_from_data(ray.direction).abs_diff_eq(cone.direction, 1e-6),
        "aim disagrees: crate {:?} vs renderer {:?}",
        world_from_data(ray.direction),
        cone.direction
    );
    assert!(
        world_from_data(ray.origin).abs_diff_eq(cone.position, 1e-6),
        "unauthored geometry must reproduce today's origin exactly"
    );
}

#[test]
fn switching_on_aperture_depth_would_move_the_origin() {
    // The size of the pending change, stated rather than discovered later: a
    // 14-degree mover's beam currently starts 0.2 m behind where its lens is.
    let cells = vec![Vec3::ZERO];
    let mount = fixture_kinematics::Mount::from_stored(Vec3::from(MOUNT_POSITION), MOUNT_ROTATION);
    let art = fixture_kinematics::Articulation::from_degrees(PAN_DEG, TILT_DEG);
    let bare = fixture_kinematics::beam_ray(
        &fixture_kinematics::FixtureGeometry::unauthored(cells.clone()),
        &mount,
        &art,
        0,
    );
    let lensed = fixture_kinematics::beam_ray(
        &fixture_kinematics::FixtureGeometry::from_class(
            fixture_kinematics::FixtureClass::Beam,
            cells,
        ),
        &mount,
        &art,
        0,
    );
    let shift = (lensed.origin - bare.origin).length();
    assert!(
        (shift - 0.2).abs() < 1e-5,
        "aperture shift should be the class depth, got {shift}"
    );
    // It moves *along the beam*, not sideways.
    assert!((lensed.origin - bare.origin)
        .normalize()
        .abs_diff_eq(bare.direction, 1e-5));
}

#[test]
fn contract_vectors_hold_in_this_workspace() {
    contract_vectors::assert_all();
}

#[test]
fn the_mirror_between_the_two_worlds_is_its_own_inverse() {
    // The mirror `coords::world_from_data` names: if this ever stops holding it
    // has grown into a real transform and the pose helpers need revisiting.
    let v = Vec3::new(0.3, -1.7, 4.1);
    assert_eq!(world_from_data(world_from_data(v)), v);
    assert!((Mat3::from_diagonal(Vec3::new(1.0, -1.0, 1.0)).determinant() + 1.0).abs() < 1e-6);
}
