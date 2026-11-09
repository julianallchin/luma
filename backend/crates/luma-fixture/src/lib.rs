//! A `luma-fixture/0` spec (`<id>.luma.json`), and everything Luma needs to
//! draw and drive it: meshes on a node tree, joints, canonical cells, sources
//! and lenses.
//!
//! ```
//! let text = r#"{"format": "luma-fixture/0", "meta": {"id": "demo/par"}, "template": "luminaire",
//!   "dims": {"size": [0.2, 0.25, 0.2]},
//!   "rig": {"mount": {"type": "none"}, "joints": {},
//!           "body": {"shape": "can", "w": 0.2, "len": 0.25, "d": 0.2, "off": 0, "tilt_y": 0.125}},
//!   "faces": [{"on": "front", "root": {"cell": true, "emitter": {"kind": "lens", "colour": "rgbw"}}}]}"#;
//! let fixture = luma_fixture::Fixture::from_json(text).unwrap();
//! let built = luma_fixture::build(&fixture).unwrap();
//! assert_eq!(built.cells.len(), 1);
//! // A static can is centred on the origin and lights downwards.
//! let (lo, hi) = built.bounds().unwrap();
//! assert!((lo + hi).length() < 1e-4);
//! let head = &built.heads[0];
//! let worlds = built.world_matrices(&built.rest_angles());
//! assert!(worlds[head.node].transform_vector3(head.beam).y < -0.99);
//! ```
//!
//! The geometry is a port of the fixtures repository's reference builder
//! (`tools/reference_builder/build.py` and `nodes.py`); `fixture-parity`
//! (this crate's `src/bin`) holds the two to the same answers over the
//! library.
//!
//! # Housing space
//!
//! Luma's bundled housings are glTF Y-up meshes whose origin is the fixture's
//! pan/tilt pivot, hung: the clamp at the top (`+Y`), a moving head's beam
//! pointing `-Y` at rest. The spec is authored standing: Y up, base at
//! `y = 0`, beam `+Y`, front `+Z`. A build comes out in housing space through
//! one transform,
//!
//! ```text
//! housing_from_spec = T(-pivot) · Rz(π)
//! ```
//!
//! the turn the spec's own `mount.hang` uses (upside down about Z, so the
//! front stays `+Z` and left and right swap, as they do when a light is hung),
//! and then the pivot moved to the origin. A spec that already hangs
//! (`mount.hang`, `array.carrier.hang`) is built hung and gets no turn.
//!
//! The pivot is the fixture's own tilt axis on the pan axis (the tilt joint's
//! origin), or the pan joint's origin when it only pans. A fixture without
//! either — a static bar, a deflector, whose joints move a mirror — is
//! centred on the middle of its box, which is where Luma centres a static
//! housing's dimension box today.
//!
//! Meshes are built with every joint at 0, so a luminaire's beam leaves along
//! `-Y`, the mount normal Luma's kinematics rests on. A manual or fixed tilt's
//! `rest` (often 90: facing front) is on its [`Joint`], not in the meshes;
//! [`Built::rest_angles`] and [`Built::world_matrices`] pose it.
//!
//! In housing space a spec's pan axis `+Y` reads `-Y` and its tilt axis `+X`
//! reads `-X`: the joints carry their axes, so the sense of a positive angle
//! is the fixture's, not a convention a caller has to know.

#![warn(clippy::pedantic)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::missing_errors_doc,
    clippy::module_name_repetitions,
    clippy::cast_lossless,
    clippy::single_match_else,
    // The port keeps the reference builder's arithmetic as written: Python's
    // `==` on floats, `(a + b) / 2`, and `min(max(..))` that tolerates crossed
    // bounds where `clamp` would panic.
    clippy::float_cmp,
    clippy::manual_midpoint,
    clippy::manual_clamp
)]

mod built;
mod face;
mod head;
mod mesh;
mod mount;
mod nodes;
mod rig;
pub mod control;
pub mod spec;

pub use built::{
    bounds_at, Built, Cell, Emitter, EmitterShape, Head, Joint, JointKind, Material, Mesh, Node,
    Source,
};
pub use rig::BuildError;
pub use spec::{Fixture, MaterialSlot};

/// Why a spec could not be read or built.
#[derive(Debug)]
pub enum Error {
    /// Not JSON, or a field of the wrong type or outside the vocabulary.
    Parse(serde_json::Error),
    /// `format` is not `luma-fixture/0`.
    Format(String),
    /// The spec parsed but describes something the builder cannot make
    /// (the reference builder draws its fallback box for these).
    Build(BuildError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "not a luma-fixture/0 spec: {e}"),
            Self::Format(v) => write!(f, "format is {v:?}, not \"luma-fixture/0\""),
            Self::Build(BuildError(why)) => write!(f, "cannot build: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// Build a fixture's meshes, joints, cells, sources and lenses in housing
/// space. Deterministic: the same spec gives the same output, bit for bit.
///
/// # Errors
/// [`Error::Build`] when the spec is self-contradictory (a zone whose
/// `cell_ids` run short, a part without its size, a preset that does not
/// exist).
pub fn build(f: &Fixture) -> Result<Built, Error> {
    let mut b = rig::Rig::new();
    match f.template {
        spec::Template::Array => mount::build_array(&mut b, f),
        _ => mount::build_luminaire(&mut b, &mount::fixture_chain(f)),
    }
    .map_err(Error::Build)?;
    if b.items.is_empty() {
        b.box_("fallback", [0.2; 3], [0.0, 0.1, 0.0], MaterialSlot::Body);
    }
    Ok(built::assemble(f, b))
}
