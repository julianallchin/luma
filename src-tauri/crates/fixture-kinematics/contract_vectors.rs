//! Pinned input → output vectors for [`fixture_kinematics`], shared verbatim by
//! both cargo workspaces.
//!
//! `src-tauri/` and `gpui/` are separate workspaces that cannot share a test
//! crate, and "the app and the renderer place beams in the same spot" is exactly
//! the property that quietly stopped holding three times before. So this file is
//! plain data plus one assertion routine, included by both sides with
//! `#[path = "…/contract_vectors.rs"] mod contract_vectors;` and run as a test
//! there. It depends only on `fixture_kinematics` and `glam`, which both
//! workspaces already have.
//!
//! Regenerating these numbers is a behaviour change, not a maintenance chore: if
//! a code change makes them fail, the code changed what a beam does.

#![allow(dead_code)]
// Pinned numbers, written out. A stored Euler term that happens to equal PI is
// data here, not an approximation of a constant a reader should reach for.
#![allow(clippy::approx_constant)]

use fixture_kinematics::{
    beam_ray, rig_position, Articulation, FixtureClass, FixtureGeometry, Mount,
};
use glam::Vec3;

/// One pinned case: everything needed to build the inputs, and every number the
/// two entry points must produce from them.
pub struct ContractVector {
    /// What this case is checking, in words.
    pub name: &'static str,
    /// `"unauthored"`, or a [`FixtureClass`] spelled in lowercase.
    pub class: &'static str,
    /// Cell offsets in the fixture frame, metres.
    pub cells: &'static [[f32; 3]],
    /// Mounting-origin → pivot offset, metres.
    pub pivot_offset: [f32; 3],
    /// Mounting origin in data space, metres.
    pub mount_position: [f32; 3],
    /// Stored Euler triple, radians.
    pub mount_rotation: [f32; 3],
    /// Which cell to ask about — deliberately allowed to exceed `cells`.
    pub cell: usize,
    /// Pan, degrees.
    pub pan_deg: f32,
    /// Tilt, degrees.
    pub tilt_deg: f32,
    /// Expected `rig_position`.
    pub rig_position: [f32; 3],
    /// Expected `beam_ray(..).origin`.
    pub beam_origin: [f32; 3],
    /// Expected `beam_ray(..).direction`.
    pub beam_direction: [f32; 3],
}

/// Rebuild this case's geometry.
///
/// # Panics
/// If `class` is not a name this file writes.
#[must_use]
pub fn geometry(v: &ContractVector) -> FixtureGeometry {
    let cells: Vec<Vec3> = v.cells.iter().copied().map(Vec3::from).collect();
    let class = match v.class {
        "unauthored" => None,
        "panel" => Some(FixtureClass::Panel),
        "wash" => Some(FixtureClass::Wash),
        "beam" => Some(FixtureClass::Beam),
        "spot" => Some(FixtureClass::Spot),
        "profile" => Some(FixtureClass::Profile),
        other => panic!("unknown fixture class {other:?}"),
    };
    match class {
        Some(c) => FixtureGeometry::from_class(c, cells),
        None => FixtureGeometry::unauthored(cells),
    }
    .with_pivot_offset(Vec3::from(v.pivot_offset))
}

/// Check every vector. The two workspaces call this and must agree to 1e-6.
///
/// # Panics
/// On any disagreement, naming the case and the axis.
pub fn assert_all() {
    for v in CONTRACT_VECTORS {
        let geom = geometry(v);
        let mount = Mount::from_stored(Vec3::from(v.mount_position), v.mount_rotation);
        let art = Articulation::from_degrees(v.pan_deg, v.tilt_deg);
        let ray = beam_ray(&geom, &mount, &art, v.cell);
        let checks = [
            (
                "rig_position",
                rig_position(&geom, &mount, v.cell),
                v.rig_position,
            ),
            ("beam origin", ray.origin, v.beam_origin),
            ("beam direction", ray.direction, v.beam_direction),
        ];
        for (what, got, want) in checks {
            assert!(
                got.abs_diff_eq(Vec3::from(want), 1e-6),
                "{}: {what} is {got:?}, pinned as {want:?}",
                v.name
            );
        }
    }
}

/// The pinned set. Covers: no articulation; every rotation axis; measured
/// aperture depths; an offset pivot; a multi-cell bar; an out-of-range cell
/// index, which is defined behaviour rather than a panic; and the three mount
/// normals the "beam = mount normal" rule turns on — hung, floor-standing, and
/// clamped to a downstage face.
pub const CONTRACT_VECTORS: &[ContractVector] = &[
    ContractVector {
        name: "parked par, square to the rig",
        class: "unauthored",
        cells: &[[0.0, 0.0, 0.0]],
        pivot_offset: [0.0, 0.0, 0.0],
        mount_position: [0.0, 0.0, 5.0],
        mount_rotation: [0.0, 0.0, 0.0],
        cell: 0,
        pan_deg: 0.0,
        tilt_deg: 0.0,
        rig_position: [0.0, 0.0, 5.0],
        beam_origin: [0.0, 0.0, 5.0],
        beam_direction: [0.0, 0.0, -1.0],
    },
    ContractVector {
        name: "mover on a raked truss, panned and tilted",
        class: "unauthored",
        cells: &[[0.0, 0.0, 0.0]],
        pivot_offset: [0.0, 0.0, 0.0],
        mount_position: [-3.5, 2.25, 6.0],
        mount_rotation: [0.35, -0.85, 1.2],
        cell: 0,
        pan_deg: 47.0,
        tilt_deg: -23.0,
        rig_position: [-3.5, 2.25, 6.0],
        beam_origin: [-3.5, 2.25, 6.0],
        beam_direction: [-0.5672991, 0.545198, -0.61719596],
    },
    ContractVector {
        name: "spot with measured aperture depth, tilted flat",
        class: "spot",
        cells: &[[0.0, 0.0, 0.0]],
        pivot_offset: [0.0, 0.0, 0.0],
        mount_position: [1.0, -2.0, 7.5],
        mount_rotation: [0.0, 0.0, 0.0],
        cell: 0,
        pan_deg: 0.0,
        tilt_deg: 90.0,
        rig_position: [1.0, -2.0, 7.23],
        beam_origin: [1.0, -1.73, 7.5],
        beam_direction: [0.0, 1.0, 4.371139e-8],
    },
    ContractVector {
        name: "profile with an offset pivot and a rotated mount",
        class: "profile",
        cells: &[[0.0, 0.0, 0.0]],
        pivot_offset: [0.02, -0.04, -0.18],
        mount_position: [2.0, 1.0, 4.0],
        mount_rotation: [-0.6, 0.4, -1.1],
        cell: 0,
        pan_deg: 123.0,
        tilt_deg: 18.0,
        rig_position: [2.130557, 1.3893647, 3.7289834],
        beam_origin: [2.20794, 1.3943596, 3.78724],
        beam_direction: [0.42626178, 0.8226177, -0.37629926],
    },
    ContractVector {
        name: "pixel bar, fourth pixel, rolled about its own axis",
        class: "panel",
        cells: &[
            [-0.45, 0.0, 0.0],
            [-0.15, 0.0, 0.0],
            [0.15, 0.0, 0.0],
            [0.45, 0.0, 0.0],
        ],
        pivot_offset: [0.0, 0.0, 0.0],
        mount_position: [0.0, 3.0, 1.2],
        mount_rotation: [1.5707964, 0.0, 0.0],
        cell: 3,
        pan_deg: 0.0,
        tilt_deg: 0.0,
        rig_position: [0.45, 2.94, 1.2],
        beam_origin: [0.45, 2.94, 1.2],
        beam_direction: [0.0, -1.0, 4.371139e-8],
    },
    ContractVector {
        name: "pixel bar, cell index past the end, clamps to the last",
        class: "panel",
        cells: &[[-0.45, 0.0, 0.0], [0.45, 0.0, 0.0]],
        pivot_offset: [0.0, 0.0, 0.0],
        mount_position: [0.0, 3.0, 1.2],
        mount_rotation: [0.0, 0.0, 0.0],
        cell: 17,
        pan_deg: 0.0,
        tilt_deg: 0.0,
        rig_position: [0.45, 3.0, 1.1400001],
        beam_origin: [0.45, 3.0, 1.1400001],
        beam_direction: [0.0, 0.0, -1.0],
    },
    ContractVector {
        name: "wash panned 180 and tilted flat",
        class: "wash",
        cells: &[[0.0, 0.0, 0.0]],
        pivot_offset: [0.0, 0.0, 0.0],
        mount_position: [0.0, -4.0, 0.35],
        mount_rotation: [0.0, 0.0, 0.0],
        cell: 0,
        pan_deg: 180.0,
        tilt_deg: 90.0,
        rig_position: [0.0, -4.0, 0.25],
        beam_origin: [-8.742278e-9, -4.1, 0.35],
        beam_direction: [-8.742278e-8, -1.0, 4.371139e-8],
    },
    ContractVector {
        name: "beam under a full three-axis mount rotation",
        class: "beam",
        cells: &[[0.03, -0.01, 0.02]],
        pivot_offset: [0.0, 0.01, -0.09],
        mount_position: [-1.75, 5.5, 3.25],
        mount_rotation: [0.9, -1.4, 0.55],
        cell: 0,
        pan_deg: -66.0,
        tilt_deg: 41.0,
        rig_position: [-1.9724854, 5.5256863, 3.0962453],
        beam_origin: [-1.9150629, 5.660558, 3.1248753],
        beam_direction: [-0.58141613, 0.7783171, -0.23701887],
    },
    ContractVector {
        name: "hung with no rotation: the mount normal is straight down",
        class: "unauthored",
        cells: &[[0.0, 0.0, 0.0]],
        pivot_offset: [0.0, 0.0, 0.0],
        mount_position: [1.25, 0.5, 6.4],
        mount_rotation: [0.0, 0.0, 0.0],
        cell: 0,
        pan_deg: 0.0,
        tilt_deg: 0.0,
        rig_position: [1.25, 0.5, 6.4],
        beam_origin: [1.25, 0.5, 6.4],
        beam_direction: [0.0, 0.0, -1.0],
    },
    ContractVector {
        name: "floor-standing uplighter: the mount normal is up",
        class: "unauthored",
        cells: &[[0.0, 0.0, 0.0]],
        pivot_offset: [0.0, 0.0, 0.0],
        mount_position: [2.0, -3.0, 0.15],
        mount_rotation: [3.1415927, 0.0, 0.0],
        cell: 0,
        pan_deg: 0.0,
        tilt_deg: 0.0,
        rig_position: [2.0, -3.0, 0.15],
        beam_origin: [2.0, -3.0, 0.15],
        beam_direction: [0.0, 8.742278e-8, 1.0],
    },
    ContractVector {
        name: "clamped to a truss's downstage face: the mount normal is the house",
        class: "unauthored",
        cells: &[[0.0, 0.0, 0.0]],
        pivot_offset: [0.0, 0.0, 0.0],
        mount_position: [0.0, 4.0, 3.4],
        mount_rotation: [1.5707964, 0.0, 0.0],
        cell: 0,
        pan_deg: 0.0,
        tilt_deg: 0.0,
        rig_position: [0.0, 4.0, 3.4],
        beam_origin: [0.0, 4.0, 3.4],
        beam_direction: [0.0, -1.0, 4.371139e-8],
    },
    ContractVector {
        // Sign, not magnitude: a positive stored `rot[2]` swings a cell on +X
        // toward the house. The mirror between the two spaces is exactly what
        // makes that read backwards, and an `abs()` in a test cannot see it.
        name: "positive stored yaw swings a cell toward the house",
        class: "unauthored",
        cells: &[[0.5, 0.0, 0.0]],
        pivot_offset: [0.0, 0.0, 0.0],
        mount_position: [0.0, 0.0, 3.0],
        mount_rotation: [0.0, 0.0, 1.5707964],
        cell: 0,
        pan_deg: 0.0,
        tilt_deg: 0.0,
        rig_position: [-2.1855694e-8, -0.5, 3.0],
        beam_origin: [-2.1855694e-8, -0.5, 3.0],
        beam_direction: [0.0, 0.0, -1.0],
    },
];
