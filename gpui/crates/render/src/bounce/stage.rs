//! A frame's stage as the bake wants it, and the bake's result as the GPU
//! wants it.

use std::sync::atomic::AtomicBool;

use glam::{UVec3, Vec3};
use luma_scene::bvh::TriMesh;

use super::bake::{self, Baked, Face, Settings};
use crate::frame::{Draw, Frame};

/// A triangle narrower than this, in metres, carries no discs: a tube's
/// facet, a bolt. What it is part of takes its bounce from the air.
const MIN_WIDTH_M: f32 = 0.1;
/// A triangle smaller than this, in square metres, carries no discs.
const MIN_AREA_M2: f32 = 0.02;
/// How far past the stage the ground carries discs when no walls end it.
const GROUND_MARGIN_M: f32 = 4.0;
/// Most air points along each axis: the air texture's size.
pub(crate) const AIR_MAX: UVec3 = UVec3::new(32, 32, 16);
/// Air points a stage gets when it is big enough to want more.
const AIR_POINTS: f32 = 1500.0;
/// Most cells in the grid that finds discs.
const GRID_CELLS: f32 = 1_000_000.0;
/// The share of a square `spacing` across that a disc stands for, on
/// average: a triangle is halved until its pieces are under three quarters
/// of one.
const PIECE_SHARE: f32 = 0.45;
/// Discs a relight workgroup lights (`bounce_relight.wgsl`).
pub(crate) const BLOCK: usize = 64;

/// What a quality level spends on the bounce.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Budget {
    /// Metres between discs on a small stage.
    pub spacing: f32,
    /// About the most discs on any stage: a big one gets them further
    /// apart.
    pub discs: f32,
    /// [`Settings::error`].
    pub error: f32,
    /// Output pixels per texel of the low pictures, along each axis.
    pub step: u32,
}

/// A stage's static geometry, taken from a frame.
pub(crate) struct Input {
    /// The big surfaces, which carry discs.
    faces: Vec<Face>,
    /// Everything that stands in light's way.
    positions: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
    lo: Vec3,
    hi: Vec3,
    settings: Settings,
}

/// The draws light bounces among: every opaque one but the lights' own
/// bodies, which move, and the room's panels the camera hides.
pub(crate) fn draws(frame: &Frame, opaque: usize) -> impl Iterator<Item = &Draw> {
    frame
        .draws
        .iter()
        .take(opaque)
        .chain(&frame.room_hidden)
        .filter(|draw| !crate::frame::is_luminaire(frame, draw))
}

/// The stage of `frame`. `None` when there is nothing to bounce light off.
pub(crate) fn input(frame: &Frame, opaque: usize, budget: Budget) -> Option<Input> {
    let mut faces = Vec::new();
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    let mut ground = None;
    let mut walled = false;
    for draw in draws(frame, opaque) {
        let mesh = &frame.meshes[draw.mesh];
        let material = &draw.material;
        let matte = material.base_color * (1.0 - material.metallic);
        let f0 = Vec3::splat(0.04).lerp(material.base_color, material.metallic);
        if crate::frame::is_ground(&mesh.key) {
            // The ground is drawn out to the horizon: it is cut to the stage
            // below, once the stage's size is known.
            let colour = frame
                .floor
                .map_or(matte, |floor| crate::floor::mean_color(floor.floor));
            ground = Some((
                draw.model.transform_point3(Vec3::ZERO).z,
                colour,
                material.roughness,
            ));
            continue;
        }
        walled |= mesh.key.starts_with(crate::frame::ROOM_PANEL);
        let first = positions.len() as u32;
        for vertex in mesh.vertices.iter() {
            let p = draw.model.transform_point3(Vec3::from(vertex.position));
            lo = lo.min(p);
            hi = hi.max(p);
            positions.push(p);
        }
        for tri in mesh.indices.chunks_exact(3) {
            triangles.push([first + tri[0], first + tri[1], first + tri[2]]);
            let mut corners = [tri[0], tri[1], tri[2]].map(|i| positions[(first + i) as usize]);
            // Wound the way the vertex normals say: stage models are not
            // reliably wound.
            let facing: Vec3 = tri
                .iter()
                .map(|&i| {
                    draw.model
                        .transform_vector3(Vec3::from(mesh.vertices[i as usize].normal))
                })
                .sum();
            let across = (corners[1] - corners[0]).cross(corners[2] - corners[0]);
            if across.dot(facing) < 0.0 {
                corners.swap(1, 2);
            }
            let area = across.length() * 0.5;
            let longest = (0..3)
                .map(|k| corners[k].distance(corners[(k + 1) % 3]))
                .fold(0.0, f32::max);
            if area >= MIN_AREA_M2 && 2.0 * area / longest.max(1e-6) >= MIN_WIDTH_M {
                faces.push(Face {
                    corners,
                    matte,
                    f0,
                    roughness: material.roughness,
                    ground: false,
                });
            }
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    if let Some((z, matte, roughness)) = ground {
        let margin = if walled { 0.0 } else { GROUND_MARGIN_M };
        let (a, b) = (lo.truncate() - margin, hi.truncate() + margin);
        let corners = [
            Vec3::new(a.x, a.y, z),
            Vec3::new(b.x, a.y, z),
            Vec3::new(b.x, b.y, z),
            Vec3::new(a.x, b.y, z),
        ];
        let first = positions.len() as u32;
        positions.extend(corners);
        for [i, j, k] in [[0, 1, 2], [0, 2, 3]] {
            triangles.push([first + i, first + j, first + k]);
            faces.push(Face {
                corners: [
                    corners[i as usize],
                    corners[j as usize],
                    corners[k as usize],
                ],
                matte,
                f0: Vec3::splat(0.04),
                roughness,
                ground: true,
            });
        }
        lo = lo.min(corners[0]);
        hi = hi.max(corners[2]);
    }
    if faces.is_empty() {
        return None;
    }
    let area: f32 = faces
        .iter()
        .map(|face| {
            (face.corners[1] - face.corners[0])
                .cross(face.corners[2] - face.corners[0])
                .length()
                * 0.5
        })
        .sum();
    Some(Input {
        faces,
        positions,
        triangles,
        lo,
        hi,
        settings: Settings {
            spacing: budget
                .spacing
                .max((area / (PIECE_SHARE * budget.discs)).sqrt()),
            error: budget.error,
        },
    })
}

/// A grid of points: its first corner, the metres between points, and how
/// many along each axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Lattice {
    pub origin: Vec3,
    pub step: f32,
    pub dims: UVec3,
}

/// A baked stage, with what finds its discs and lights them.
pub(crate) struct Stage {
    pub baked: Baked,
    /// The cells that find the discs round a point, and each cell's discs
    /// ([`bake::grid`]).
    pub cells: Lattice,
    pub cell_discs: Vec<u32>,
    /// The ball round each [`BLOCK`] of discs: xyz its centre, w its radius.
    pub balls: Vec<[f32; 4]>,
    /// The points in the air.
    pub air: Lattice,
}

/// The points in the air over a stage from `lo` to `hi`: a metre apart on a
/// small one, further on a big one.
fn air(lo: Vec3, hi: Vec3) -> Lattice {
    // Off the walls and the floor, which a point on them would look along.
    let inset = 0.1;
    let extent = (hi - lo - 2.0 * inset).max(Vec3::splat(0.1));
    let mut step = (extent.x * extent.y * extent.z / AIR_POINTS)
        .cbrt()
        .max(1.0);
    step = step.max((extent / (AIR_MAX - 1).as_vec3()).max_element());
    Lattice {
        origin: lo + inset,
        step,
        dims: ((extent / step).ceil().as_uvec3() + 1).min(AIR_MAX),
    }
}

/// Bake `input`. `None` when `stop` was raised before the end.
pub(crate) fn build(input: Input, stop: &AtomicBool) -> Option<Stage> {
    let Input {
        faces,
        positions,
        triangles,
        lo,
        hi,
        settings,
    } = input;
    let blockers = TriMesh::new(positions, triangles);
    let air = air(lo, hi);
    let points: Vec<Vec3> = (0..air.dims.z)
        .flat_map(|z| {
            (0..air.dims.y).flat_map(move |y| (0..air.dims.x).map(move |x| UVec3::new(x, y, z)))
        })
        .map(|at| air.origin + at.as_vec3() * air.step)
        .collect();
    let baked = bake::bake(&faces, &blockers, &points, settings, stop)?;
    let reach = baked
        .discs
        .iter()
        .map(|disc| disc.reach)
        .fold(0.0, f32::max);
    let (lo, hi) = (lo - reach, hi + reach);
    let extent = hi - lo;
    let cell = settings
        .spacing
        .max((extent.x * extent.y * extent.z / GRID_CELLS).cbrt());
    let cells = Lattice {
        origin: lo,
        step: cell,
        dims: (extent / cell).ceil().as_uvec3().max(UVec3::ONE),
    };
    let cell_discs = bake::grid(&baked, cells.origin, cells.dims, cells.step);
    let balls = baked
        .discs
        .chunks(BLOCK)
        .map(|block| {
            let centre =
                block.iter().map(|disc| Vec3::from(disc.pos)).sum::<Vec3>() / block.len() as f32;
            let radius = block
                .iter()
                .map(|disc| centre.distance(Vec3::from(disc.pos)) + disc.radius)
                .fold(0.0, f32::max);
            centre.extend(radius).to_array()
        })
        .collect();
    Some(Stage {
        baked,
        cells,
        cell_discs,
        balls,
        air,
    })
}
