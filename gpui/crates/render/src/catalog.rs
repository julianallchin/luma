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
use luma_scene::venue::{Node, NodeKind, NodeSockets, Params};
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

// ---------------------------------------------------------------------------
// The venue graph's view of the same geometry
// ---------------------------------------------------------------------------

/// The parameters a node's generator is standing at.
///
/// A placed node overrides the palette default one key at a time, so an absent
/// `span` means "the default span" rather than zero — which is why this reads
/// through [`default_params`] rather than constructing a [`Procedural`] from
/// scratch. `faces` is the generator's own bitmask, stored as a number because
/// `venue_node_params` is `(key, value)` and a second encoding for one column
/// would be a second thing to keep true.
#[must_use]
pub fn node_params(family: Family, params: &Params) -> Procedural {
    match (family, default_params(family)) {
        (Family::Truss, Procedural::Truss { span }) => Procedural::Truss {
            #[allow(clippy::cast_possible_truncation)]
            span: params.get("span", f64::from(span)) as f32,
        },
        (Family::Hinge, Procedural::Hinge { angle }) => Procedural::Hinge {
            #[allow(clippy::cast_possible_truncation)]
            angle: params.get("angle", f64::from(angle)) as f32,
        },
        (Family::Corner, Procedural::Corner { faces }) => Procedural::Corner {
            faces: params.lookup("faces").map_or(faces, face_set_from_bits),
        },
        // `default_params` is total over `Family`, so the pairs above are
        // exhaustive; matching on both halves is what makes that checkable
        // rather than asserted.
        (_, other) => other,
    }
}

/// A [`FaceSet`] out of the number `venue_node_params` holds, and back.
///
/// One bit per [`Face`], in [`Face::ALL`] order. Out-of-range bits are dropped
/// rather than refused: `Corner::new` widens anything under two ways to a
/// through-box, so every input names a corner that exists.
#[must_use]
pub fn face_set_from_bits(bits: f64) -> FaceSet {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bits = bits.round().clamp(0.0, 63.0) as u32;
    FaceSet::of(
        Face::ALL
            .into_iter()
            .enumerate()
            .filter(|(i, _)| bits & (1 << i) != 0)
            .map(|(_, f)| f),
    )
}

/// The inverse of [`face_set_from_bits`] — what `set_params` writes.
#[must_use]
pub fn face_set_bits(faces: FaceSet) -> f64 {
    f64::from(
        Face::ALL
            .into_iter()
            .enumerate()
            .filter(|(_, f)| faces.contains(*f))
            .fold(0_u32, |bits, (i, _)| bits | (1 << i)),
    )
}

/// Sockets for a venue-graph node: authored against a GLB's bbox, or read off
/// the generator's end frames **at the node's own parameters**.
///
/// [`CatalogSockets`] answers the same question for a *catalog* entry, at the
/// palette default. A placed 6 m truss has its ends 6 m apart, so the resolver
/// cannot use that answer, and this is the wrapper that supplies the difference.
pub struct VenueSockets {
    catalog: CatalogSockets,
}

impl VenueSockets {
    /// Resolve the catalog once, against the GLBs under `meshes_root`.
    ///
    /// # Errors
    /// As [`CatalogSockets::load`].
    pub fn load(meshes_root: impl Into<std::path::PathBuf>) -> anyhow::Result<Self> {
        Ok(Self {
            catalog: CatalogSockets::load(meshes_root)?,
        })
    }

    /// The catalog view, for the drag-time solver.
    #[must_use]
    pub fn catalog(&self) -> &CatalogSockets {
        &self.catalog
    }
}

impl NodeSockets for VenueSockets {
    fn sockets(&self, node: &Node) -> Vec<ResolvedSocket> {
        let Some(catalog_ref) = node.catalog_ref.as_deref() else {
            return Vec::new();
        };
        // A fixture's `catalog_ref` is a `fixtures` row id, not a piece: it
        // hangs off its host's socket and needs only one of its own to hang by.
        if node.kind == NodeKind::Fixture {
            return vec![fixture_clamp()];
        }
        match luma_scene::catalog::piece(catalog_ref).map(|p| p.geometry) {
            Some(Geometry::Procedural(family)) => {
                procedural_sockets(node_params(family, &node.params))
            }
            _ => self.catalog.sockets(catalog_ref).to_vec(),
        }
    }
}

/// The one socket every fixture has: the clamp, on its underside.
///
/// A fixture is not a catalog piece — it is a row in the patch — and its
/// housing geometry is the QLC+ definition's business, not the snap solver's.
/// One `EquipmentMount` at the origin is the whole of what placing it needs,
/// and [`luma_scene::venue`] turns the *host* socket's normal into the beam.
pub fn fixture_clamp() -> ResolvedSocket {
    ResolvedSocket::from_frame(
        FIXTURE_CLAMP_SOCKET,
        SocketType::EquipmentMount,
        DVec3::ZERO,
        DVec3::NEG_Y,
        DVec3::X,
    )
}

/// The name of the socket [`fixture_clamp`] declares.
pub const FIXTURE_CLAMP_SOCKET: &str = "clamp";

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
