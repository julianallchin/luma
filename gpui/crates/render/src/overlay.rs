//! Editor affordances: the selection cage and the translate gizmo.
//!
//! Spec §2.3 calls these `Unlit` — flat colour, no lighting, optionally no
//! depth test. Spec §5.2 owns the interactive half (hit testing, drag frames);
//! this module owns only what the affordances *look* like, which is the half
//! the goldens pin.
//!
//! Both shapes are transliterated rather than reinvented, because a gizmo that
//! is "basically right" is worse than none: `fixture-object.tsx` renders the
//! cage as a wireframe `BoxGeometry` at the fixture's physical dimensions, and
//! `<TransformControls>` resolves to `three-stdlib`'s translate gizmo at
//! `size = 0.5`. The handle table, the constant-screen-size factor and the
//! edge-on hide/flip rules below are that implementation, term for term.

use glam::{Mat4, Vec3};

use crate::coords::{euler_xyz, hex_srgb};
use crate::frame::{Camera, Definitions, MeshData};
use crate::scene_desc::Scene;

/// How an overlay sits against the scene it is drawn over. The two variants are
/// the two material configurations three.js uses, not a free combination:
/// `MeshBasicMaterial` defaults for the cage, and the gizmo's
/// `depthTest: false, depthWrite: false, transparent: true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayDepth {
    /// Tested and written like scene geometry, drawn opaque.
    Tested,
    /// Ignores depth entirely and alpha-blends over whatever is there. Three's
    /// `renderOrder = Infinity` puts these last; so does their position in
    /// [`crate::Frame::overlays`].
    Free,
}

/// One unlit draw. `mesh` indexes [`crate::Frame::meshes`], the same bank the
/// lit draws use — only the topology and the shading differ.
pub struct Overlay {
    /// Index into [`crate::Frame::meshes`].
    pub mesh: usize,
    /// Model-to-world transform.
    pub model: Mat4,
    /// Whether `mesh.indices` is a line list rather than a triangle list.
    pub lines: bool,
    /// Linear RGB, three's `material.color`.
    pub color: Vec3,
    /// three's `material.opacity`.
    pub opacity: f32,
    /// Depth and blend behaviour.
    pub depth: OverlayDepth,
}

/// `TransformControls.size`, as `unified-transform.tsx` passes it.
const GIZMO_SIZE: f32 = 0.5;

/// Handles hide when their axis points within this much of straight at the
/// camera, where the drag math degenerates (`AXIS_HIDE_TRESHOLD`).
const AXIS_HIDE: f32 = 0.99;

/// Plane handles hide when seen too close to edge-on (`PLANE_HIDE_TRESHOLD`).
const PLANE_HIDE: f32 = 0.2;

/// Which of the three axes a handle's name mentions, in `X`, `Y`, `Z` order.
type Axes = [bool; 3];

/// Which end of an axis a handle belongs to. Three tags the two arrowheads
/// `fwd`/`bwd` and shows whichever points away from the camera-facing side;
/// untagged handles (lines, plane quads) mirror instead.
#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Fwd,
    Bwd,
    Both,
}

/// One entry of three's gizmo table, before the per-frame hide/flip pass.
struct Handle {
    axes: Axes,
    /// Set for the three single-axis handles; drives `AXIS_HIDE`.
    axis_only: Option<usize>,
    /// Set for the three plane handles: the index of the plane's normal axis;
    /// drives `PLANE_HIDE`.
    plane_normal: Option<usize>,
    end: End,
    mesh: MeshKind,
    local: Mat4,
    color: Vec3,
    opacity: f32,
}

/// The five primitives the gizmo is built from. Interning them by kind is what
/// keeps eleven handles down to five uploads.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum MeshKind {
    /// `CylinderGeometry(0, 0.05, 0.2, 12)` — an arrowhead.
    Arrow,
    /// The unit segment `(0,0,0) -> (1,0,0)`.
    Segment,
    /// `OctahedronGeometry(0.1, 0)`.
    Octahedron,
    /// `PlaneGeometry(0.295, 0.295)`.
    Quad,
}

impl MeshKind {
    fn key(self) -> &'static str {
        match self {
            MeshKind::Arrow => "::gizmo-arrow",
            MeshKind::Segment => "::gizmo-segment",
            MeshKind::Octahedron => "::gizmo-octahedron",
            MeshKind::Quad => "::gizmo-quad",
        }
    }

    fn build(self) -> MeshData {
        match self {
            MeshKind::Arrow => cone(0.05, 0.2, 12),
            MeshKind::Segment => MeshData {
                key: String::new(),
                vertices: vec![vertex(Vec3::ZERO), vertex(Vec3::X)].into(),
                indices: vec![0, 1].into(),
            },
            MeshKind::Octahedron => octahedron(0.1),
            MeshKind::Quad => crate::frame::plane_mesh(0.295, 0.295),
        }
    }

    fn lines(self) -> bool {
        self == MeshKind::Segment
    }
}

/// Every editor affordance the scene asks for, in draw order.
///
/// `bank` interns the geometry into the frame's shared mesh table.
pub(crate) fn build(
    scene: &Scene,
    definitions: &Definitions,
    camera: &Camera,
    to_world: Mat4,
    bank: &mut crate::frame::Bank,
) -> Vec<Overlay> {
    let mut out = Vec::new();
    if !scene.editing || scene.selected_fixture_ids.is_empty() {
        return out;
    }

    // --- selection cages ---------------------------------------------------
    // `fixture-object.tsx`: a wireframe box at the definition's physical
    // dimensions, bright yellow for the primary and olive for the rest. Note
    // the missing-dimension default is zero here, not the 300 mm
    // `extractPhysicalDimensions` uses — a definition with no `Physical` block
    // gets a degenerate cage in the app too.
    for (i, id) in scene.selected_fixture_ids.iter().enumerate() {
        let Some(fixture) = scene.fixtures.iter().find(|f| &f.id == id) else {
            continue;
        };
        let Some(def) = definitions.get(&fixture.fixture_path) else {
            continue;
        };
        let d = def.physical.as_ref().and_then(|p| p.dimensions.as_ref());
        let size = d.map_or(Vec3::ZERO, |d| {
            Vec3::new(d.width, d.height, d.depth) / 1000.0
        });
        if size.min_element() <= 0.0 {
            continue;
        }
        let mesh = bank.insert(format!("::cage:{}", fixture.fixture_path), || {
            box_wireframe(size)
        });
        out.push(Overlay {
            mesh,
            model: to_world * fixture_transform(fixture),
            lines: true,
            color: hex_srgb(if i == 0 { 0xff_ff_00 } else { 0xb8_b8_46 }),
            opacity: 1.0,
            depth: OverlayDepth::Tested,
        });
    }

    // --- translate gizmo ---------------------------------------------------
    // One widget on an empty pivot node, parked at the primary's position with
    // identity rotation (`restingPivotPosition`, `space = "world"`).
    let Some(primary) = scene
        .fixtures
        .iter()
        .find(|f| Some(&f.id) == scene.selected_fixture_ids.first())
    else {
        return out;
    };
    let pivot_three = Vec3::new(primary.pos[0], primary.pos[2], primary.pos[1]);
    let pivot_world = to_world.transform_point3(pivot_three);

    // Constant screen size: `factor * size / 7` where `factor` is the distance
    // to the camera times a field-of-view term, capped at 7.
    let distance = (camera.eye - pivot_world).length();
    let fov_term = (1.9 * (camera.fov_y_deg.to_radians() / 2.0).tan()).min(7.0);
    let scale = distance * fov_term * GIZMO_SIZE / 7.0;

    // `eye` is expressed in the gizmo's own (three) space, where the world axes
    // are the unit vectors the hide/flip rules test against.
    let eye_world = (camera.eye - pivot_world).normalize_or_zero();
    let eye = to_world.transpose().transform_vector3(eye_world);
    let dots = [eye.x, eye.y, eye.z];

    for handle in translate_handles() {
        if handle.axis_only.is_some_and(|a| dots[a].abs() > AXIS_HIDE)
            || handle
                .plane_normal
                .is_some_and(|a| dots[a].abs() < PLANE_HIDE)
        {
            continue;
        }
        // Per-axis: the far-side arrowhead is dropped, and everything else
        // mirrors so the widget always reaches toward the viewer.
        let mut flip = Vec3::ONE;
        let mut hidden = false;
        for (a, &present) in handle.axes.iter().enumerate() {
            if !present {
                continue;
            }
            if dots[a] < 0.0 {
                match handle.end {
                    End::Fwd => hidden = true,
                    End::Bwd | End::Both => flip[a] = -flip[a],
                }
            } else if handle.end == End::Bwd {
                hidden = true;
            }
        }
        if hidden {
            continue;
        }
        let mesh = bank.insert(handle.mesh.key().to_string(), || handle.mesh.build());
        out.push(Overlay {
            mesh,
            model: to_world
                * Mat4::from_translation(pivot_three)
                * Mat4::from_scale(flip * scale)
                * handle.local,
            lines: handle.mesh.lines(),
            color: handle.color,
            opacity: handle.opacity,
            depth: OverlayDepth::Free,
        });
    }

    out
}

/// Position and Euler rotation of a fixture, in three space — the same
/// composition `frame::build` uses for the fixture body.
fn fixture_transform(fixture: &crate::scene_desc::Fixture) -> Mat4 {
    Mat4::from_translation(Vec3::new(fixture.pos[0], fixture.pos[2], fixture.pos[1]))
        * Mat4::from_mat3(euler_xyz(fixture.rot[0], fixture.rot[2], fixture.rot[1]))
}

/// `three-stdlib`'s `gizmoTranslate`, flattened into the child order
/// `setupGizmo` produces (entries reversed within each name, names in
/// declaration order). Draw order is the transparent sort order: every handle
/// sits at the same world position, so three's depth sort is a no-op and the
/// list order is what paints.
fn translate_handles() -> Vec<Handle> {
    const X: Axes = [true, false, false];
    const Y: Axes = [false, true, false];
    const Z: Axes = [false, false, true];
    const XY: Axes = [true, true, false];
    const YZ: Axes = [false, true, true];
    const XZ: Axes = [true, false, true];
    const XYZ: Axes = [true, true, true];

    let red = hex_srgb(0xff_00_00);
    let green = hex_srgb(0x00_ff_00);
    let blue = hex_srgb(0x00_00_ff);
    let yellow = hex_srgb(0xff_ff_00);
    let cyan = hex_srgb(0x00_ff_ff);
    let magenta = hex_srgb(0xff_00_ff);
    let white = hex_srgb(0xff_ff_ff);

    let axis = |axes: Axes, i: usize| Handle {
        axes,
        axis_only: Some(i),
        plane_normal: None,
        end: End::Both,
        mesh: MeshKind::Segment,
        local: Mat4::IDENTITY,
        color: Vec3::ZERO,
        opacity: 1.0,
    };
    // Plane-handle tick marks are the unit segment squashed to 12.5% length.
    let tick = |axes: Axes, pos: Vec3, rot: Vec3, color: Vec3| Handle {
        axes,
        axis_only: None,
        plane_normal: None,
        end: End::Both,
        mesh: MeshKind::Segment,
        local: Mat4::from_translation(pos)
            * Mat4::from_mat3(euler_xyz(rot.x, rot.y, rot.z))
            * Mat4::from_scale(Vec3::new(0.125, 1.0, 1.0)),
        color,
        opacity: 1.0,
    };
    let quad = |axes: Axes, normal: usize, pos: Vec3, rot: Vec3, color: Vec3| Handle {
        axes,
        axis_only: None,
        plane_normal: Some(normal),
        end: End::Both,
        mesh: MeshKind::Quad,
        local: Mat4::from_translation(pos) * Mat4::from_mat3(euler_xyz(rot.x, rot.y, rot.z)),
        color,
        opacity: 0.25,
    };
    let arrow = |axes: Axes, i: usize, end: End, pos: Vec3, rot: Vec3, color: Vec3| Handle {
        axes,
        axis_only: Some(i),
        plane_normal: None,
        end,
        mesh: MeshKind::Arrow,
        local: Mat4::from_translation(pos) * Mat4::from_mat3(euler_xyz(rot.x, rot.y, rot.z)),
        color,
        opacity: 1.0,
    };

    let q = std::f32::consts::FRAC_PI_2;
    let pi = std::f32::consts::PI;
    vec![
        Handle {
            color: red,
            ..axis(X, 0)
        },
        arrow(X, 0, End::Bwd, Vec3::X, Vec3::new(0.0, 0.0, q), red),
        arrow(X, 0, End::Fwd, Vec3::X, Vec3::new(0.0, 0.0, -q), red),
        Handle {
            color: green,
            local: Mat4::from_mat3(euler_xyz(0.0, 0.0, q)),
            ..axis(Y, 1)
        },
        arrow(Y, 1, End::Bwd, Vec3::Y, Vec3::new(pi, 0.0, 0.0), green),
        arrow(Y, 1, End::Fwd, Vec3::Y, Vec3::ZERO, green),
        Handle {
            color: blue,
            local: Mat4::from_mat3(euler_xyz(0.0, -q, 0.0)),
            ..axis(Z, 2)
        },
        arrow(Z, 2, End::Bwd, Vec3::Z, Vec3::new(-q, 0.0, 0.0), blue),
        arrow(Z, 2, End::Fwd, Vec3::Z, Vec3::new(q, 0.0, 0.0), blue),
        Handle {
            axes: XYZ,
            axis_only: None,
            plane_normal: None,
            end: End::Both,
            mesh: MeshKind::Octahedron,
            local: Mat4::IDENTITY,
            color: white,
            opacity: 0.25,
        },
        tick(
            XY,
            Vec3::new(0.3, 0.18, 0.0),
            Vec3::new(0.0, 0.0, q),
            yellow,
        ),
        tick(XY, Vec3::new(0.18, 0.3, 0.0), Vec3::ZERO, yellow),
        quad(XY, 2, Vec3::new(0.15, 0.15, 0.0), Vec3::ZERO, yellow),
        tick(YZ, Vec3::new(0.0, 0.3, 0.18), Vec3::new(0.0, -q, 0.0), cyan),
        tick(YZ, Vec3::new(0.0, 0.18, 0.3), Vec3::new(0.0, 0.0, q), cyan),
        quad(
            YZ,
            0,
            Vec3::new(0.0, 0.15, 0.15),
            Vec3::new(0.0, q, 0.0),
            cyan,
        ),
        tick(
            XZ,
            Vec3::new(0.3, 0.0, 0.18),
            Vec3::new(0.0, -q, 0.0),
            magenta,
        ),
        tick(XZ, Vec3::new(0.18, 0.0, 0.3), Vec3::ZERO, magenta),
        quad(
            XZ,
            1,
            Vec3::new(0.15, 0.0, 0.15),
            Vec3::new(-q, 0.0, 0.0),
            magenta,
        ),
    ]
}

fn vertex(p: Vec3) -> crate::assets::Vertex {
    crate::assets::Vertex {
        position: p.to_array(),
        normal: [0.0; 3],
        uv: [0.0; 2],
        tangent: [1.0, 0.0, 0.0, 1.0],
    }
}

/// three's `BoxGeometry` in `wireframe` mode: the six faces are built
/// independently and each is two triangles, so every face contributes four
/// border edges *and* the diagonal between its triangles. Those diagonals are
/// visible in the goldens; a plain twelve-edge box is the wrong picture.
fn box_wireframe(size: Vec3) -> MeshData {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    // `buildPlane(u, v, w, udir, vdir, ...)` for each face, in three's order.
    let faces: [(usize, usize, usize, f32, f32, f32); 6] = [
        (2, 1, 0, -1.0, -1.0, 1.0),
        (2, 1, 0, 1.0, -1.0, -1.0),
        (0, 2, 1, 1.0, 1.0, 1.0),
        (0, 2, 1, 1.0, -1.0, -1.0),
        (0, 1, 2, 1.0, -1.0, 1.0),
        (0, 1, 2, -1.0, -1.0, -1.0),
    ];
    for (u, v, w, udir, vdir, wsign) in faces {
        let base = vertices.len() as u32;
        // Corner order is `(ix, iy)` row-major over a 1x1 grid.
        for (iy, ix) in [(0.0, 0.0), (0.0, 1.0), (1.0, 0.0), (1.0, 1.0)] {
            let mut p = Vec3::ZERO;
            p[u] = (ix - 0.5) * size[u] * udir;
            p[v] = (iy - 0.5) * size[v] * vdir;
            p[w] = 0.5 * size[w] * wsign;
            vertices.push(vertex(p));
        }
        // Triangles (a, b, d) and (b, c, d) with a=0, b=2, c=3, d=1; the shared
        // edge 1-2 is the diagonal, and `checkEdge` dedups it to one segment.
        for (i, j) in [(0, 2), (2, 1), (1, 0), (2, 3), (3, 1)] {
            indices.extend([base + i, base + j]);
        }
    }
    MeshData {
        key: String::new(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
}

/// `CylinderGeometry(0, radius, height, segments, 1, false)`. The zero top
/// radius collapses the torso to a fan from the apex; the closed end keeps its
/// cap, which is what makes the arrowhead read as solid from below.
fn cone(radius: f32, height: f32, segments: u32) -> MeshData {
    let half = height / 2.0;
    let mut vertices = vec![
        vertex(Vec3::new(0.0, half, 0.0)),
        vertex(Vec3::new(0.0, -half, 0.0)),
    ];
    for i in 0..segments {
        let theta = i as f32 / segments as f32 * std::f32::consts::TAU;
        vertices.push(vertex(Vec3::new(
            radius * theta.sin(),
            -half,
            radius * theta.cos(),
        )));
    }
    let mut indices = Vec::new();
    for i in 0..segments {
        let a = 2 + i;
        let b = 2 + (i + 1) % segments;
        indices.extend([0, a, b]);
        indices.extend([1, b, a]);
    }
    MeshData {
        key: String::new(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
}

/// `OctahedronGeometry(radius, 0)`: the six axis poles, eight faces, wound
/// outward.
fn octahedron(radius: f32) -> MeshData {
    let poles = [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z];
    let vertices: Vec<_> = poles.iter().map(|&p| vertex(p * radius)).collect();
    let mut indices = Vec::new();
    for (x, y, z) in [
        (0, 2, 4),
        (0, 4, 3),
        (0, 3, 5),
        (0, 5, 2),
        (1, 4, 2),
        (1, 3, 4),
        (1, 5, 3),
        (1, 2, 5),
    ] {
        indices.extend([x, y, z]);
    }
    MeshData {
        key: String::new(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
}
