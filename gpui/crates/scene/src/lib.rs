//! The venue model, and the scene math and editor logic for the wgpu renderer
//! — no GPU, no windowing.
//!
//! The venue graph and its resolver, the stage catalog and the sockets it
//! resolves against the shipped meshes ([`catalog`], [`truss`], [`mesh`]), the
//! stored venue and fixture records ([`environment`], [`definition`]), the
//! retained scene graph, the camera, CPU raycasting, and the socket/snap
//! solver. The backend's models, database and DMX output depend on this crate
//! for venue types, so nothing here may link a GPU; `luma-render` draws what
//! this describes.
//!
//! # Two conventions live here, deliberately
//!
//! **Renderer space is Z-up, right-handed, f32** (spec §2.1) — [`graph`],
//! [`camera`], [`bvh`]. It matches the database and the eval engine, and it is
//! what the GPU consumes.
//!
//! **The socket layer is asset space, Y-up, f64** — [`sockets`], [`snap`].
//! Sockets are authored against a mesh's bounding box in the mesh's own frame,
//! and glTF mandates Y-up, so this is the frame the authored data is already
//! in. `f64` because the solver is editor math, not GPU math: the golden
//! vectors pin it to 1e-6 absolute over coordinates up to 1e6, which is two
//! orders of magnitude past what f32 can hold.
//!
//! The one place the socket layer touches *world* space is the implicit ground
//! plane ([`snap::WORLD_UP`]). See its doc comment — that constant, plus a
//! conversion at the scene boundary, is the whole of what a future move of the
//! socket layer to Z-up would touch.

pub mod aabb;
pub mod build;
pub mod bvh;
pub mod camera;
pub mod catalog;
pub mod coords;
pub mod definition;
pub mod distribute;
pub mod environment;
pub mod face;
pub mod framing;
pub mod gesture;
pub mod gizmo;
pub mod golden;
pub mod graph;
pub mod luminaire;
pub mod mesh;
pub mod navigate;
pub mod patch;
pub mod selection;
pub mod snap;
pub mod sockets;
pub mod strobe;
pub mod truss;
pub mod venue;
pub mod venue_tiles;

pub use aabb::Aabb;
pub use build::{Extent, Footprint, MODULE_M};
pub use bvh::{MeshBvh, Ray, RayHit, TriMesh};
pub use camera::{Camera, UnknownView, View, EYE_HEIGHT_M};
pub use catalog::{piece, pieces, Family, Geometry, PaletteGroup, Piece, PieceKind};
pub use framing::{Beam, Framing, Insets, Viewfinder};
pub use gesture::{ClickOrbit, ClickOrbitRelease, ClickOrbitUpdate, Marquee, ScreenRect};
pub use gizmo::{
    apply_rotation, apply_translation, gizmo_scale, hit_test_gizmo, selection_pivot, snap_angle_15,
    Axis, GizmoHandle, GizmoHit, GizmoMode, PivotMode, TransformTarget, RING_RADIUS,
};
pub use graph::{
    MaterialHandle, MeshHandle, Node, NodeContent, NodeFlags, NodeId, SceneGraph, Transform,
};
pub use navigate::{FlyKeys, Surface, SurfaceHit, ZoomLimits};
pub use selection::Selection;
pub use snap::{solve_snap, Aim, ScenePiece, SnapInput, SnapMatch, SnapResult, SnapSurface};
pub use sockets::{
    BboxAnchor, Polarity, ResolvedSocket, RollFreedom, SocketDef, SocketKind, SocketMode,
    SocketType,
};
// The venue graph is reached as `luma_scene::venue::*` rather than re-exported:
// its `Node` is a node of the *relation* tree and `graph::Node` is a node of the
// *scene* tree, and flattening both into one namespace would make the two
// indistinguishable at every call site.

/// The repository root, for the bins and tests that read tracked data.
///
/// The crate sits at `<repo>/gpui/crates/scene`.
#[must_use]
pub fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crate is three levels below the repo root")
        .to_path_buf()
}

/// The repository's mesh catalog, `resources/meshes`, for the bins and tests
/// that draw tracked assets.
#[must_use]
pub fn meshes_root() -> std::path::PathBuf {
    repo_root().join("resources/meshes")
}
