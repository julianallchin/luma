//! Unlit affordances: the aim arrows, the selection cage, the transform gizmo,
//! and what the venue builder is about to do (`scene_desc::Build`).
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

use glam::{Mat3, Mat4, Vec3};
use luma_scene::{gizmo_scale, Axis, GizmoHandle, GizmoMode, RING_RADIUS};

use crate::assets::Library;
use crate::coords::{euler_xyz, hex_srgb, three_from_data, three_from_world, three_pose_from_data};
use crate::frame::{Camera, Definitions, MeshData};
use crate::scene_desc::{Geometry, Scene, SocketMarkState};

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

/// The builder's "yes": the one accent the venue builder paints with, shared by
/// a compatible socket and a run that fits.
const ACCENT: u32 = 0x6b_7d_ff;

/// The builder's "no", on every affordance that can be refused at once — a
/// refused ghost and its refused run are one answer, not two.
/// How solid an accepted ghost is. High enough to read over a dark stage,
/// low enough that the room behind it is still visible through it — a preview
/// the operator has to hunt for is not one.
const GHOST_ALPHA: f32 = 0.5;

/// A refusal is drawn harder than an acceptance: it has to stop the hand.
const GHOST_REFUSED_ALPHA: f32 = 0.65;

const REFUSED: u32 = 0xff_3b_30;

/// A socket bead's radius before the constant-screen-size factor. A quarter of
/// the gizmo's octahedron, because a socket is a place to aim at, not a handle
/// to grab.
const SOCKET_RADIUS: f32 = 0.025;

/// How solid the selected-piece tint is — under the ghost's alpha, because a
/// selection is a fact about what is already there, not a preview.
const SELECTED_PIECE_ALPHA: f32 = 0.3;

/// How much of a bead survives being occluded: the x-ray copy's share of the
/// visible copy's alpha.
const BEAD_XRAY: f32 = 0.35;

/// Half-length of a measure end tick, before the same factor.
const TICK_RADIUS: f32 = 0.06;

/// How far an aim arrow reaches out of the fixture, in metres.
///
/// A fixed length rather than a throw traced to the first surface: the arrow
/// answers "which way", and a ray cast per fixture per frame would buy a length
/// nobody reads at the price of a BVH over the whole room. Three metres is
/// longer than any housing and shorter than the smallest room, so a rig of
/// them reads as a field of directions rather than as a thicket.
const AIM_LENGTH_M: f32 = 3.0;

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
/// `bank` interns the geometry into the frame's shared mesh table; `lib`
/// resolves the ghost's authored mesh, the one affordance whose shape comes
/// from an asset rather than from this module.
pub(crate) fn build(
    scene: &Scene,
    definitions: &Definitions,
    camera: &Camera,
    to_world: Mat4,
    pivot: Option<Vec3>,
    lib: &mut Library,
    bank: &mut crate::frame::Bank,
) -> Vec<Overlay> {
    let mut out = Vec::new();
    if scene.aim_arrows {
        out.extend(aim_arrows(scene, definitions, camera, to_world, bank));
    }
    if !scene.editing {
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
            model: to_world * three_pose_from_data(fixture.pos, fixture.rot),
            lines: true,
            color: hex_srgb(if i == 0 { 0xff_ff_00 } else { 0xb8_b8_46 }),
            opacity: 1.0,
            depth: OverlayDepth::Tested,
        });
    }

    // --- selected stage pieces ---------------------------------------------
    // A piece has no physical-dimensions block to cage, so the selection is
    // said by drawing the piece itself again, unlit, in the cage's own colour
    // — a tint that reads through the room the way the ghost does. Every
    // selected piece gets it, snapped or free: selection and grab-ability are
    // two facts, and this is the first one.
    for id in &scene.editor.selected_piece_ids {
        let Some(piece) = scene.pieces.iter().find(|p| &p.id == id) else {
            continue;
        };
        let model = to_world
            * three_pose_from_data(piece.pos, piece.rot)
            * Mat4::from_scale(Vec3::splat(piece.scale));
        let draws =
            crate::frame::piece_draws(&piece.geometry, model, lib, bank, None).unwrap_or_default();
        out.extend(draws.into_iter().map(|draw| Overlay {
            mesh: draw.mesh,
            model: draw.model,
            lines: false,
            color: hex_srgb(0xff_ff_00),
            opacity: SELECTED_PIECE_ALPHA,
            depth: OverlayDepth::Free,
        }));
    }

    // --- builder affordances -----------------------------------------------
    // Sockets first, then the ghost over them, then the measure over both:
    // painted in the order of how much they say about the *next* click. All
    // three ignore depth, so a joint behind a truss still reads.
    let build = &scene.editor.build;
    for mark in &build.sockets {
        let pos = three_from_data(Vec3::from(mark.pos));
        let world = to_world.transform_point3(pos);
        let scale = gizmo_scale((camera.eye - world).length(), camera.fov_y_deg);
        let (color, opacity) = match mark.state {
            SocketMarkState::Open => (0x6e_6e_6e, 0.5),
            SocketMarkState::Compatible => (ACCENT, 0.9),
            SocketMarkState::Latched => (0xff_ff_ff, 1.0),
        };
        let model = to_world
            * Mat4::from_translation(pos)
            * Mat4::from_mat3(basis_from_up(three_from_data(Vec3::from(mark.normal))))
            * Mat4::from_scale(scale * Vec3::new(1.0, 0.6, 1.0));
        // Twice: solid where the joint is visible, faint through what hides
        // it. A bead at full strength through a wall reads as *on* the wall;
        // no bead at all loses the joint the moment the camera dips.
        for (depth, alpha) in [
            (OverlayDepth::Tested, opacity),
            (OverlayDepth::Free, opacity * BEAD_XRAY),
        ] {
            out.push(Overlay {
                mesh: bank.insert("::socket-bead".to_string(), || octahedron(SOCKET_RADIUS)),
                // Squashed along the joint's own normal, so the bead reads as
                // a face on the piece rather than as a free-floating handle.
                model,
                lines: false,
                color: hex_srgb(color),
                opacity: alpha,
                depth,
            });
        }
    }

    for ghost in &build.ghosts {
        let root = to_world
            * three_pose_from_data(ghost.pos, ghost.rot)
            * Mat4::from_scale(Vec3::splat(ghost.scale));
        // A ghost whose asset will not load draws nothing: the same missing
        // mesh is already absent from the room, and a placement preview is not
        // worth failing a frame over.
        let draws =
            crate::frame::piece_draws(&ghost.geometry, root, lib, bank, None).unwrap_or_default();
        out.extend(draws.into_iter().map(|draw| Overlay {
            mesh: draw.mesh,
            model: draw.model,
            lines: false,
            color: hex_srgb(if ghost.refused { REFUSED } else { 0xff_ff_ff }),
            // The refusal is drawn *harder* than the acceptance: a placement
            // that will not commit has to stop the hand, not fade out of it.
            // Both are well clear of the room behind them — a ghost the
            // operator has to hunt for over a dark stage is not a preview.
            opacity: if ghost.refused {
                GHOST_REFUSED_ALPHA
            } else {
                GHOST_ALPHA
            },
            depth: OverlayDepth::Free,
        }));
    }

    // The extend ray, with a tick at each end so a run the pointer has not
    // dragged yet is still a mark on the room rather than nothing. The metres
    // readout belongs beside it — that is a gpui element the app projects onto
    // the viewport, and there is deliberately no text in this layer.
    if let Some(measure) = &build.measure {
        let from = three_from_data(Vec3::from(measure.from));
        let to = three_from_data(Vec3::from(measure.to));
        let color = hex_srgb(if measure.refused { REFUSED } else { ACCENT });
        let run = basis_from_up(to - from);
        out.push(Overlay {
            mesh: bank.insert(MeshKind::Segment.key().to_string(), || {
                MeshKind::Segment.build()
            }),
            // The unit segment lies along +X, so the run itself is the X column;
            // the other two only have to be perpendicular.
            model: to_world
                * Mat4::from_translation(from)
                * Mat4::from_mat3(Mat3::from_cols(to - from, run.x_axis, run.z_axis)),
            lines: true,
            color,
            opacity: 1.0,
            depth: OverlayDepth::Free,
        });
        for end in [from, to] {
            let world = to_world.transform_point3(end);
            let scale = gizmo_scale((camera.eye - world).length(), camera.fov_y_deg);
            out.push(Overlay {
                mesh: bank.insert("::measure-tick".to_string(), tick_cross),
                model: to_world
                    * Mat4::from_translation(end)
                    * Mat4::from_scale(Vec3::splat(scale * TICK_RADIUS)),
                lines: true,
                color,
                opacity: 1.0,
                depth: OverlayDepth::Free,
            });
        }
    }

    // --- transform gizmo ---------------------------------------------------
    // One widget on an empty pivot node, parked on the selection's anchor with
    // identity rotation (`restingPivotPosition`, `space = "world"`).
    let Some(pivot_world) = pivot else {
        return out;
    };
    let pivot_three = to_world.transpose().transform_point3(pivot_world);
    let scale = gizmo_scale((camera.eye - pivot_world).length(), camera.fov_y_deg);

    // `eye` is expressed in the gizmo's own (three) space, where the world axes
    // are the unit vectors the hide/flip rules test against.
    let eye_world = (camera.eye - pivot_world).normalize_or_zero();
    let eye = to_world.transpose().transform_vector3(eye_world);
    let dots = [eye.x, eye.y, eye.z];

    if scene.editor.gizmo == GizmoMode::Rotate {
        out.extend(rotate_rings(
            pivot_three,
            scale,
            eye,
            scene.editor.hover,
            to_world,
            bank,
        ));
        return out;
    }

    let hover = scene.editor.hover;
    for handle in translate_handles() {
        let hovered = hover.is_some_and(|hover| handle_hovered(&handle, hover));
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
            // three's hover rule: the picked handle turns yellow whole —
            // shaft, arrowheads, ticks and quad together — so what will move
            // is named before the press.
            color: if hovered {
                hex_srgb(0xff_ff_00)
            } else {
                handle.color
            },
            opacity: if hovered { 1.0 } else { handle.opacity },
            depth: OverlayDepth::Free,
        });
    }

    out
}

/// Whether one drawn primitive belongs to the handle the pointer is on.
///
/// The pick names a handle; the drawing is several primitives — a line and two
/// arrowheads, or two ticks and a quad. What they share is their `axes`
/// signature, which is what makes "light the whole handle" one comparison.
fn handle_hovered(handle: &Handle, hover: GizmoHandle) -> bool {
    let index = |axis: Axis| match axis {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    };
    match hover {
        GizmoHandle::TranslateAxis(axis) => handle.axis_only == Some(index(axis)),
        // A plane handle's primitives all name the two in-plane axes and never
        // a single one — which is exactly not the axis lines' signature.
        GizmoHandle::TranslatePlane(normal) => {
            handle.axis_only.is_none()
                && !handle.axes[index(normal)]
                && handle.axes.iter().filter(|&&p| p).count() == 2
        }
        GizmoHandle::TranslateScreen => handle.axes == [true, true, true],
        // Rings are drawn by `rotate_rings`, which lights its own.
        GizmoHandle::RotateAxis(_) | GizmoHandle::RotateScreen => false,
    }
}

/// One arrow per emitting fixture, from its mount point along the beam it
/// leaves at rest.
///
/// The direction is [`crate::luminaire::beam_direction`] with no pinned
/// position — the one answer to "which way is this pointing", so an arrow can
/// never disagree with the cone drawn under it. A fixture that answers with the
/// zero vector has no beam (a hazer, a definition the catalogue lost) and gets
/// no arrow: an arrow out of something that does not light is a lie.
///
/// Rest, not the score's aim at `t`: this is a question about how the rig is
/// hung, and a head swung somewhere by a cue would answer a different one.
///
/// The shaft is a world-length segment so the reach is readable in metres; the
/// head is constant screen size, the same [`gizmo_scale`] every other mark in
/// this module wears, so it stays legible from across the room.
fn aim_arrows(
    scene: &Scene,
    definitions: &Definitions,
    camera: &Camera,
    to_world: Mat4,
    bank: &mut crate::frame::Bank,
) -> Vec<Overlay> {
    let mut out = Vec::new();
    let color = hex_srgb(ACCENT);
    for fixture in &scene.fixtures {
        let direction = crate::luminaire::beam_direction(
            definitions.get(&fixture.fixture_path),
            fixture.rot,
            None,
        );
        let Some(direction) = direction.try_normalize().map(three_from_world) else {
            continue;
        };
        let origin = three_from_data(Vec3::from(fixture.pos));
        let tip = origin + direction * AIM_LENGTH_M;
        // The unit segment lies along +X, so the run is the X column and the
        // other two only have to be perpendicular — as the measure ray does.
        let run = basis_from_up(direction);
        out.push(Overlay {
            mesh: bank.insert(MeshKind::Segment.key().to_string(), || {
                MeshKind::Segment.build()
            }),
            model: to_world
                * Mat4::from_translation(origin)
                * Mat4::from_mat3(Mat3::from_cols(tip - origin, run.x_axis, run.z_axis)),
            lines: true,
            color,
            opacity: 1.0,
            depth: OverlayDepth::Free,
        });
        // The cone's apex is +Y in its own space, which `run` points along.
        let scale = gizmo_scale(
            (camera.eye - to_world.transform_point3(tip)).length(),
            camera.fov_y_deg,
        );
        out.push(Overlay {
            mesh: bank.insert(MeshKind::Arrow.key().to_string(), || {
                MeshKind::Arrow.build()
            }),
            model: to_world
                * Mat4::from_translation(tip)
                * Mat4::from_mat3(run)
                * Mat4::from_scale(Vec3::splat(scale)),
            lines: false,
            color,
            opacity: 1.0,
            depth: OverlayDepth::Free,
        });
    }
    out
}

/// The world point the gizmo stands on, or `None` with nothing selected.
///
/// Ported from `unified-transform.tsx`'s `stagePieceAnchorWorld`, which picked
/// these to match what a hand expects to grab:
///
/// * **fixture** — its own origin, which is where its body is.
/// * **stage piece** — the bottom centre of its mesh's bounds. Stage GLBs put
///   their local origin at a corner, so the origin parks the widget off in
///   space; bottom centre sits in the middle of the footprint, on the surface
///   the piece rests on.
/// * **several of either** — the mean of their anchors.
///
/// The third React rule, a *parented* piece anchoring on the socket that
/// attaches it to its parent, is not portable yet and is deliberately absent:
/// a [`crate::scene_desc::Piece`] arrives flattened, with no parent, and the
/// socket catalogue is still TypeScript-only. Both land in the venue graph
/// (`docs/design/venue-graph.md`, phases 2–3).
pub(crate) fn pivot(scene: &Scene, lib: &mut Library, to_world: Mat4) -> Option<Vec3> {
    let fixtures = scene.selected_fixture_ids.iter().filter_map(|id| {
        let fixture = scene.fixtures.iter().find(|f| &f.id == id)?;
        Some(to_world.transform_point3(three_from_data(Vec3::from(fixture.pos))))
    });
    let pieces = scene.editor.gizmo_piece_ids.iter().filter_map(|id| {
        let piece = scene.pieces.iter().find(|p| &p.id == id)?;
        let model = to_world
            * three_pose_from_data(piece.pos, piece.rot)
            * Mat4::from_scale(Vec3::splat(piece.scale));
        Some(model.transform_point3(footprint_centre(&piece.geometry, lib)))
    });
    let (sum, count) = fixtures
        .chain(pieces)
        .fold((Vec3::ZERO, 0u32), |(sum, n), p| (sum + p, n + 1));
    (count > 0).then(|| sum / count as f32)
}

/// Bottom centre of a piece's bounds, in its own (three) space.
///
/// A procedural family has no loaded mesh to measure and answers with its
/// origin, which is already where it is built from.
fn footprint_centre(geometry: &Geometry, lib: &mut Library) -> Vec3 {
    let Geometry::MeshPath(path) = geometry else {
        return Vec3::ZERO;
    };
    let Ok(glb) = lib.get(path) else {
        return Vec3::ZERO;
    };
    let (lo, hi) = glb.bounds();
    Vec3::new(lo.x.midpoint(hi.x), lo.y, lo.z.midpoint(hi.z))
}

/// The rotate widget: one ring per axis, plus the screen-facing ring.
///
/// Rings rather than three's fuller `gizmoRotate` because these are the four
/// shapes [`luma_scene::hit_test_gizmo`] picks, at the radius it picks them —
/// the axis rings in the gizmo's own space, where three's X/Y/Z become world
/// X/Z/Y under the mirror, and a ring is the same circle either way round.
fn rotate_rings(
    pivot_three: Vec3,
    scale: f32,
    eye: Vec3,
    hover: Option<GizmoHandle>,
    to_world: Mat4,
    bank: &mut crate::frame::Bank,
) -> Vec<Overlay> {
    let mesh = bank.insert("::gizmo-ring".to_string(), ring);
    let q = std::f32::consts::FRAC_PI_2;
    let axes = [
        (
            Vec3::new(0.0, q, 0.0),
            hex_srgb(0xff_00_00),
            GizmoHandle::RotateAxis(Axis::X),
        ),
        (
            Vec3::new(q, 0.0, 0.0),
            hex_srgb(0x00_ff_00),
            GizmoHandle::RotateAxis(Axis::Y),
        ),
        (
            Vec3::ZERO,
            hex_srgb(0x00_00_ff),
            GizmoHandle::RotateAxis(Axis::Z),
        ),
    ];
    // The screen ring's plane is normal to the view, so its basis is built from
    // the eye direction rather than from an axis.
    let n = eye.normalize_or(Vec3::Z);
    let u = n.cross(Vec3::Y).normalize_or(Vec3::X);
    let screen = Mat4::from_mat3(Mat3::from_cols(u, n.cross(u), n));
    axes.into_iter()
        .map(|(rot, color, handle)| {
            (
                Mat4::from_mat3(euler_xyz(rot.x, rot.y, rot.z)),
                color,
                handle,
            )
        })
        // Grey, three's `XYZE`: the screen ring is the one handle that is not
        // an axis, and yellow is the selection cage's colour — which is also
        // why yellow is what a *hovered* ring turns.
        .chain(std::iter::once((
            screen,
            hex_srgb(0x78_78_78),
            GizmoHandle::RotateScreen,
        )))
        .map(|(orientation, color, handle)| Overlay {
            mesh,
            model: to_world
                * Mat4::from_translation(pivot_three)
                * Mat4::from_scale(Vec3::splat(scale))
                * orientation,
            lines: true,
            color: if hover == Some(handle) {
                hex_srgb(0xff_ff_00)
            } else {
                color
            },
            opacity: 1.0,
            depth: OverlayDepth::Free,
        })
        .collect()
}

/// A line-list circle of [`RING_RADIUS`] in the XY plane.
fn ring() -> MeshData {
    const SEGMENTS: u32 = 64;
    let mut vertices = Vec::with_capacity(SEGMENTS as usize);
    let mut indices = Vec::with_capacity(SEGMENTS as usize * 2);
    for i in 0..SEGMENTS {
        let angle = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        vertices.push(vertex(Vec3::new(
            angle.cos() * RING_RADIUS,
            angle.sin() * RING_RADIUS,
            0.0,
        )));
        indices.extend([i, (i + 1) % SEGMENTS]);
    }
    MeshData {
        key: String::new(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
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

/// An orthonormal basis whose +Y is `up`. The other two axes are arbitrary but
/// stable, which is all a shape symmetric about that axis needs; a zero vector
/// answers with the identity rather than with NaNs.
fn basis_from_up(up: Vec3) -> Mat3 {
    let y = up.normalize_or(Vec3::Y);
    // Any seed not parallel to `y`, so the cross product has a direction.
    let seed = if y.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
    let x = y.cross(seed).normalize();
    Mat3::from_cols(x, y, x.cross(y))
}

/// Three unit segments crossed at the origin: an end mark that is visible from
/// any angle, including down the run it terminates.
fn tick_cross() -> MeshData {
    MeshData {
        key: String::new(),
        vertices: [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z]
            .map(vertex)
            .into(),
        indices: vec![0, 1, 2, 3, 4, 5].into(),
    }
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
