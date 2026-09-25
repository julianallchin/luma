//! Where the renderer puts a beam, pinned — and the same numbers
//! `fixture-kinematics` produces, checked on this workspace's toolchain.
//!
//! Two things live here, and they are deliberately separate:
//!
//! 1. **Characterization.** `frame::build` seats a moving head's lens on the
//!    front face of its drawn head: the tilt pivot the bundled mesh puts below
//!    the clamp, plus the head's depth along the beam. It used to sit at the
//!    bare mounting origin; this test was the diff for that move, and it now
//!    pins the pivot-plus-depth geometry in `fixture_kinematics` terms (a
//!    pivot offset, and an origin further along the same ray).
//!
//! 2. **Agreement.** The shared contract vectors, evaluated here rather than in
//!    `backend/`. The app and the renderer are separate cargo workspaces and
//!    cannot share a test crate, so the file is included by path from both.

use std::collections::BTreeMap;

use glam::{Mat3, Mat4, Vec3};
use luma_render::assets::Library;
use luma_render::coords::world_from_data;
use luma_render::scene_desc::{
    CameraPose, Definition, Dimensions, Fixture, Lens, Mode, Physical, PrimitiveState,
    RenderSettings, Scene,
};
use luma_render::{build_frame, Frame};

#[path = "../../../../../backend/crates/fixture-kinematics/contract_vectors.rs"]
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
                radius_m: None,
                offset_m: None,
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
        aim_arrows: false,
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

    let meshes = crate::common::meshes();
    build_frame(&scene, &definitions, 0.0, &mut Library::new(meshes))
        .expect("characterization scene should build")
}

/// The bundled moving-head mesh's tilt pivot below the clamp, in the mount's
/// own data frame (`-Z` is the rest beam), at the mover definition's size.
fn mesh_pivot_offset() -> Vec3 {
    let meshes = crate::common::meshes();
    let mut library = Library::new(meshes);
    let glb = library.get("qlc/moving_head.glb").unwrap();
    let (lo, hi) = glb.bounds();
    let scale_y = 0.42 / (hi.y - lo.y);
    let worlds = glb.world_matrices(Mat4::IDENTITY, &std::collections::HashMap::new());
    let head = worlds[glb.node_index("head").unwrap()].transform_point3(Vec3::ZERO);
    // Three space is `(x, z, y)` of data space; the pivot is on the mesh axis.
    Vec3::new(0.0, 0.0, head.y * scale_y)
}

fn pivot_ray() -> fixture_kinematics::Ray {
    let geom = fixture_kinematics::FixtureGeometry::unauthored(vec![Vec3::ZERO])
        .with_pivot_offset(mesh_pivot_offset());
    let mount = fixture_kinematics::Mount::from_stored(Vec3::from(MOUNT_POSITION), MOUNT_ROTATION);
    let art = fixture_kinematics::Articulation::from_degrees(PAN_DEG, TILT_DEG);
    fixture_kinematics::beam_ray(&geom, &mount, &art, 0)
}

#[test]
fn beam_origin_is_the_lens_in_front_of_the_tilt_pivot() {
    let frame = characterization_frame();
    let cone = frame
        .fixture_cones
        .first()
        .expect("a lit moving head should emit one cone");
    let ray = pivot_ray();
    let pivot = world_from_data(ray.origin);
    let front = cone.position - pivot;
    // On the beam, ahead of the pivot by the head's scaled depth (the mesh's
    // head reaches 0.329 units past its pivot; this head is 0.42 m tall).
    assert!(
        front.normalize().dot(cone.direction) > 0.9999,
        "the lens left the beam axis: {front:?} vs {:?}",
        cone.direction
    );
    assert!(
        (front.length() - 0.128).abs() < 2e-3,
        "lens depth {} m",
        front.length()
    );
    // And the pivot itself, literally, so the derivation cannot agree with
    // itself: 0.10 m below the clamp along the rotated mount normal.
    let clamp = world_from_data(Vec3::from(MOUNT_POSITION));
    assert!(
        ((pivot - clamp).length() - 0.1007).abs() < 1e-3,
        "{pivot:?}"
    );
}

#[test]
fn kinematics_reproduces_the_beam_direction_and_ray() {
    let frame = characterization_frame();
    let cone = frame.fixture_cones.first().expect("one cone");
    let ray = pivot_ray();
    assert!(
        world_from_data(ray.direction).abs_diff_eq(cone.direction, 1e-6),
        "aim disagrees: crate {:?} vs renderer {:?}",
        world_from_data(ray.direction),
        cone.direction
    );
    // The lens lies on the crate's pivot ray: the renderer's origin is a
    // reparameterisation along it (§15.1), not a displacement off it.
    let along = (cone.position - world_from_data(ray.origin)).dot(cone.direction);
    assert!(
        (world_from_data(ray.origin) + cone.direction * along).abs_diff_eq(cone.position, 1e-4),
        "the lens is off the crate's pivot ray"
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
