//! How long a host's face is, and which way it runs.
//!
//! [`crate::catalog`] answers *where* a socket is; this answers *how much of
//! the piece it spans*. They are the same knowledge — the measured GLB, the
//! generator's parameters — so this module sits beside the catalog rather than
//! in [`luma_scene`], which has no geometry of its own and must not grow one.
//!
//! # One rule for every host
//!
//! **A face's length is the host's bounding-box extent along the socket's
//! tangent.** A truss's side runs its span because its bbox does; a deck's top
//! runs the deck because the deck's does; the venue floor and grid are planes
//! and have no length at all. There is no per-piece table, and adding a piece
//! to the catalog adds nothing here.
//!
//! The design doc defers `span_exceeds` from `Placement` because "a piece's
//! bounds … the socket supply does not carry". This is the supply of that
//! bound, kept separate from the socket supply on purpose: a resolver that
//! never needed bounds still does not take them.

use glam::DVec3;
use luma_scene::catalog::Geometry;
use luma_scene::distribute::Feature;
use luma_scene::sockets::ResolvedSocket;
use luma_scene::venue::{root_socket, Node, NodeKind, NodeSockets};

use crate::catalog::{node_params, VenueSockets};
use crate::scene_desc::Procedural;
use crate::truss::{Truss, OUTER_M, PANEL_PITCH_M};

/// A host face, ready to lay a row of fixtures along.
#[derive(Clone, Debug)]
pub struct HostFace {
    /// The socket the row mates. `u` runs along its tangent from the face's
    /// middle, `v` along the bitangent, and its outward normal is the beam
    /// direction of everything hung on it — which is why choosing the face is
    /// the whole of choosing "hanging under" versus "standing on".
    pub socket: ResolvedSocket,
    /// How long it is and how short a change to that length the host admits.
    pub feature: Feature,
}

/// The named face of a host node, or `None` if the node has no such socket.
///
/// The root's two synthesized planes ([`luma_scene::venue::root_socket`]) are
/// answered here too, so "on the floor" and "on this truss" are one call.
#[must_use]
pub fn host_face(sockets: &VenueSockets, node: &Node, name: &str) -> Option<HostFace> {
    if node.kind == NodeKind::Venue {
        if let Some(socket) = root_socket(name) {
            return Some(HostFace {
                socket,
                feature: Feature::unbounded(),
            });
        }
    }
    let socket = sockets.sockets(node).into_iter().find(|s| s.name == name)?;
    let feature = extent_along(sockets, node, socket.tangent);
    Some(HostFace { socket, feature })
}

/// The host's size along one of its own local axes, as a distributable feature.
///
/// `None` bounds — an unrecognised piece, or one whose GLB the catalog does not
/// hold — become an unbounded feature rather than a zero-length one: a face of
/// unknown size refuses nothing, which is the honest answer, where zero would
/// refuse everything and blame the rig.
fn extent_along(sockets: &VenueSockets, node: &Node, tangent: DVec3) -> Feature {
    let Some(catalog_ref) = node.catalog_ref.as_deref() else {
        return Feature::unbounded();
    };
    match luma_scene::catalog::piece(catalog_ref).map(|p| p.geometry) {
        // A generated piece is the size its node's parameters make it, so the
        // catalog's palette-default measurement would be the wrong number for
        // every truss anybody has resized.
        Some(Geometry::Procedural(family)) => {
            procedural_feature(node_params(family, &node.params), tangent)
        }
        Some(Geometry::Mesh { .. }) => match sockets.catalog().bounds(catalog_ref) {
            Some(bbox) => {
                let size = bbox.size();
                Feature::bounded(project(size, tangent), None)
            }
            None => Feature::unbounded(),
        },
        None => Feature::unbounded(),
    }
}

/// A generated piece's extent along `tangent`.
///
/// Only the straight family has a length worth distributing along; a corner
/// block and a hinge are one block big whichever way you measure them, and a
/// row along one is a row of one.
fn procedural_feature(params: Procedural, tangent: DVec3) -> Feature {
    let size = match params {
        Procedural::Truss { span } => {
            let span = f64::from(Truss::new(span).span_m());
            DVec3::new(span, f64::from(OUTER_M) * 2.0, f64::from(OUTER_M) * 2.0)
        }
        Procedural::Corner { .. } | Procedural::Hinge { .. } => {
            DVec3::splat(f64::from(OUTER_M) * 2.0)
        }
    };
    let quantum = match params {
        // A truss is built out of whole panels, so its span is the one host
        // length a fit report can ask a caller to change.
        Procedural::Truss { .. } => Some(f64::from(PANEL_PITCH_M)),
        Procedural::Corner { .. } | Procedural::Hinge { .. } => None,
    };
    Feature::bounded(project(size, tangent), quantum)
}

/// A bbox size projected onto a unit direction: the axis-aligned extent the
/// direction picks out, which for the axis-aligned tangents every socket in
/// this catalog carries is simply that axis's size.
fn project(size: DVec3, direction: DVec3) -> f64 {
    (size * direction.abs()).max_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use luma_scene::venue::Params;

    fn sockets() -> VenueSockets {
        VenueSockets::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"),
        )
        .expect("the catalog resolves against the shipped meshes")
    }

    fn truss(span: f64) -> Node {
        Node {
            id: "run".into(),
            kind: NodeKind::Run,
            catalog_ref: Some("truss/straight".into()),
            label: None,
            params: Params::from_iter([("span".to_string(), span)]),
        }
    }

    /// The load-bearing one: a truss face is as long as the truss, at the span
    /// the *node* is standing at rather than the palette default.
    #[test]
    fn a_truss_face_is_as_long_as_its_own_span() {
        let sockets = sockets();
        for span in [1.0, 3.0, 6.0, 12.0] {
            let face =
                host_face(&sockets, &truss(span), "face_-y").expect("a stick has an underside");
            assert_eq!(face.feature.length_m, Some(span), "span {span}");
            assert_eq!(face.feature.quantum_m, Some(0.5));
        }
    }

    /// `u` on a truss face has to be metres along the run, or a distribution
    /// would lay its row across the truss instead of along it.
    #[test]
    fn a_truss_faces_tangent_is_the_span_axis() {
        let sockets = sockets();
        for name in ["face_-y", "face_+y", "face_-z", "face_+z"] {
            let face = host_face(&sockets, &truss(3.0), name).expect(name);
            assert!(
                (face.socket.tangent - DVec3::X).length() < 1e-12,
                "{name} runs {:?}",
                face.socket.tangent
            );
            assert!(
                face.socket.normal.dot(DVec3::X).abs() < 1e-12,
                "{name} faces along the span"
            );
        }
    }

    /// Beam is the mount normal, so opposite faces have to point opposite ways
    /// — that is the whole of "under the truss points down, on top points up".
    #[test]
    fn opposite_faces_point_opposite_ways() {
        let sockets = sockets();
        let under = host_face(&sockets, &truss(3.0), "face_-y").unwrap();
        let over = host_face(&sockets, &truss(3.0), "face_+y").unwrap();
        assert!((under.socket.normal + over.socket.normal).length() < 1e-12);
    }

    /// A quantized span: a node asking for 3.2 m is a 3.0 m truss, and the face
    /// is as long as the truss that exists, not as the one that was typed.
    #[test]
    fn a_face_is_as_long_as_the_truss_that_gets_built() {
        let sockets = sockets();
        let face = host_face(&sockets, &truss(3.2), "face_-y").unwrap();
        assert_eq!(face.feature.length_m, Some(3.0));
    }

    /// A deck's top is as long as the deck, measured off the same GLB its
    /// sockets were authored against.
    #[test]
    fn a_deck_top_is_as_long_as_the_deck() {
        let sockets = sockets();
        let node = Node {
            id: "deck".into(),
            kind: NodeKind::Stage,
            catalog_ref: Some("stage_lab/stage_praticavel_2x1x1.glb".into()),
            label: None,
            params: Params::default(),
        };
        let face = host_face(&sockets, &node, "top").expect("a deck has a top");
        let length = face.feature.length_m.expect("a deck is a bounded piece");
        assert!(
            (0.5..=4.0).contains(&length),
            "a 2x1 m deck measured {length} m along its top"
        );
        assert_eq!(face.feature.quantum_m, None, "a deck cannot be extended");
    }

    /// The floor is a plane: unbounded, so a row on it is refused for nothing.
    #[test]
    fn the_venue_floor_is_unbounded() {
        let sockets = sockets();
        let root = Node {
            id: "root".into(),
            kind: NodeKind::Venue,
            catalog_ref: None,
            label: None,
            params: Params::default(),
        };
        for name in ["floor", "rig"] {
            let face = host_face(&sockets, &root, name).expect(name);
            assert_eq!(face.feature.length_m, None);
        }
        assert!(host_face(&sockets, &root, "nope").is_none());
    }
}
