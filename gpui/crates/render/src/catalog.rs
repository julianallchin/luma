//! Geometry for the stage catalog: bounding boxes for the GLB pieces, end
//! frames for the generated ones, and the resolved sockets both produce.
//!
//! [`mod@luma_scene::catalog`] is the catalog — what pieces exist, what they are
//! called, where their authored sockets sit. It cannot resolve any of that on
//! its own: an authored socket is a bbox anchor and the bbox is in a file, and
//! a generated piece has no authored sockets at all. This module is where the
//! two meet, and it is the only place that knows both.
//!
//! Everything here is in the socket layer's frame — glTF Y-up, piece-local —
//! which is also `crate::truss`'s local space, so an end frame is already a
//! socket frame and needs no conversion.

use crate::assets::Library;
use crate::scene_desc::Procedural;
use crate::truss::{Corner, Face, FaceSet};
use glam::DVec3;
use luma_scene::aabb::DAabb;
use luma_scene::catalog::{pieces, Family, Geometry, Piece};
use luma_scene::snap::SocketLookup;
use luma_scene::sockets::{resolve_socket, ResolvedSocket, SocketType};
use std::collections::HashMap;

/// Palette default span for the straight truss, in metres. Quantized to whole
/// panels by [`crate::truss::Truss::new`].
pub const DEFAULT_TRUSS_SPAN_M: f32 = 3.0;

/// Palette default deflection for a hinge, in degrees.
pub const DEFAULT_HINGE_ANGLE_DEG: f32 = 90.0;

/// Palette default open faces for a corner block: an L, which is the block a
/// rig actually turns a corner with. Every other way count is the same entry
/// with more faces opened.
pub const DEFAULT_CORNER_FACES: [Face; 2] = [Face::NegX, Face::NegZ];

/// The parameters a family starts at when dragged out of the palette. A placed
/// node overrides them (`venue_node_params`, phase 3).
#[must_use]
pub fn default_params(family: Family) -> Procedural {
    match family {
        Family::Truss => Procedural::Truss {
            span: DEFAULT_TRUSS_SPAN_M,
        },
        Family::Corner => Procedural::Corner {
            faces: FaceSet::of(DEFAULT_CORNER_FACES),
        },
        Family::Hinge => Procedural::Hinge {
            angle: DEFAULT_HINGE_ANGLE_DEG,
        },
    }
}

/// The sockets of a generated piece: one per open face, plus the grab the
/// cursor follows.
///
/// Face names come from the generator's own vocabulary (`-x`, `+y`, …) rather
/// than an index, so a corner that gains a way keeps the names of the ways it
/// already had — a venue row naming `face_-x` must not start meaning a
/// different face because a sibling was opened.
#[must_use]
pub fn procedural_sockets(params: Procedural) -> Vec<ResolvedSocket> {
    let names: Vec<String> = match params {
        // The two ends of a stick, named as the ripped truss GLBs named them,
        // so a venue built before the generator landed still reads.
        Procedural::Truss { .. } => vec!["end_a".into(), "end_b".into()],
        Procedural::Corner { faces } => Corner::new(faces)
            .faces()
            .iter()
            .map(|f| format!("face_{}", f.as_str()))
            .collect(),
        Procedural::Hinge { .. } => vec!["leaf_fixed".into(), "leaf_swinging".into()],
    };
    let frames = params.end_frames();
    debug_assert_eq!(
        names.len(),
        frames.len(),
        "socket names and end frames must agree; both walk the generator's face order"
    );

    let grab =
        ResolvedSocket::from_frame("grab", SocketType::Grab, DVec3::ZERO, DVec3::Y, DVec3::X);
    std::iter::once(grab)
        .chain(names.iter().zip(frames).map(|(name, f)| {
            ResolvedSocket::from_frame(
                name,
                SocketType::TrussEnd,
                f.position.as_dvec3(),
                f.normal.as_dvec3(),
                f.up.as_dvec3(),
            )
        }))
        .collect()
}

/// Every catalog piece's sockets, resolved once.
///
/// Eager rather than lazy because [`SocketLookup`] hands out borrowed slices:
/// a cache that filled on demand would need interior mutability, and there are
/// fourteen pieces. The unresolvable case is reported at construction, so the
/// solver never has to have an opinion about a missing asset.
pub struct CatalogSockets {
    by_id: HashMap<String, Vec<ResolvedSocket>>,
}

impl CatalogSockets {
    /// Resolve the whole catalog against the GLBs under `meshes_root`.
    ///
    /// # Errors
    /// Fails if a mesh piece's GLB is missing or unreadable, or if it measures
    /// empty — a piece with no bounding box has every socket at the origin,
    /// which would snap silently and wrongly rather than loudly.
    pub fn load(meshes_root: impl Into<std::path::PathBuf>) -> anyhow::Result<Self> {
        let mut library = Library::new(meshes_root);
        let mut by_id = HashMap::new();
        for piece in pieces() {
            by_id.insert(piece.id.to_string(), resolve(piece, &mut library)?);
        }
        Ok(Self { by_id })
    }

    /// The pieces this holds sockets for, in catalog order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        pieces().iter().map(|p| p.id)
    }
}

/// One piece's sockets: authored against the measured bbox, or read off the
/// generator's end frames.
fn resolve(piece: &Piece, library: &mut Library) -> anyhow::Result<Vec<ResolvedSocket>> {
    match piece.geometry {
        Geometry::Procedural(family) => Ok(procedural_sockets(default_params(family))),
        Geometry::Mesh { path } => {
            let (lo, hi) = library.get(path)?.bounds();
            let bbox = DAabb::new(lo.as_dvec3(), hi.as_dvec3());
            anyhow::ensure!(
                bbox.size().max_element() > 0.0,
                "{path}: measured an empty bounding box"
            );
            Ok(piece
                .sockets
                .iter()
                .map(|def| resolve_socket(def, &bbox))
                .collect())
        }
    }
}

impl SocketLookup for CatalogSockets {
    fn sockets(&self, piece_id: &str) -> &[ResolvedSocket] {
        self.by_id.get(piece_id).map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meshes_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes")
    }

    /// The load-bearing one: every piece in the catalog resolves every socket
    /// it declares, against the asset that actually ships.
    #[test]
    fn every_catalog_piece_resolves_its_sockets() {
        let sockets = CatalogSockets::load(meshes_root()).expect("catalog resolves");
        for piece in pieces() {
            let resolved = sockets.sockets(piece.id);
            let want = if piece.geometry.is_procedural() {
                // grab + one per open face.
                match piece.geometry {
                    Geometry::Procedural(f) => default_params(f).end_frames().len() + 1,
                    Geometry::Mesh { .. } => unreachable!(),
                }
            } else {
                piece.sockets.len()
            };
            assert_eq!(resolved.len(), want, "{}: socket count", piece.id);
            assert!(
                resolved.iter().any(|s| s.socket_type == SocketType::Grab),
                "{}: no grab socket",
                piece.id
            );
            for s in resolved {
                assert!(
                    s.position.is_finite() && s.normal.is_normalized(),
                    "{}/{}: degenerate frame",
                    piece.id,
                    s.name
                );
            }
        }
    }

    /// Sockets are authored against the bbox, so a piece whose GLB is metres
    /// across must have sockets metres apart — this catches a mesh loaded at
    /// the wrong scale, which resolves "successfully" into a piece the size of
    /// a coin.
    #[test]
    fn deck_sockets_span_the_deck() {
        let sockets = CatalogSockets::load(meshes_root()).expect("catalog resolves");
        let deck = sockets.sockets("stage_lab/stage_praticavel_1x1.glb");
        let left = deck.iter().find(|s| s.name == "edge_left").expect("left");
        let right = deck.iter().find(|s| s.name == "edge_right").expect("right");
        let width = (right.position - left.position).length();
        assert!(
            (0.9..1.1).contains(&width),
            "1×1 m deck measured {width} m across"
        );
    }

    #[test]
    fn corner_socket_names_follow_the_open_faces() {
        let sockets = procedural_sockets(Procedural::Corner {
            faces: FaceSet::of([Face::NegX, Face::PosY, Face::PosZ]),
        });
        let names: Vec<&str> = sockets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["grab", "face_-x", "face_+y", "face_+z"]);
    }

    /// Two generated pieces mate through the same socket vocabulary the GLB
    /// pieces use: a truss end is a truss end whatever produced it.
    #[test]
    fn generated_ends_are_truss_ends() {
        for family in [Family::Truss, Family::Corner, Family::Hinge] {
            for s in procedural_sockets(default_params(family)) {
                assert!(matches!(
                    s.socket_type,
                    SocketType::Grab | SocketType::TrussEnd
                ));
            }
        }
    }
}
