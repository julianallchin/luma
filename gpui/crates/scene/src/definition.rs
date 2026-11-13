//! The QLC+ fixture definition, as far as the stage reads it: type, modes,
//! and the physical block — housing size, pixel layout and lens.
//!
//! The field names are QLC+'s own (`@Width`, `Mode`, …), because the bundle's
//! `.qxf` XML is converted to JSON with its attribute and element names kept.

use serde::{Deserialize, Serialize};

/// The QLC+ type of a laser projector.
pub const LASER_PROJECTOR: &str = "Laser Projector";

/// The QLC+ `.qxf` subset the renderer reads, or a library fixture's
/// lowering of itself into it plus its [`Built`](luma_fixture::Built).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Definition {
    /// A library (`lib:<id>`) fixture's build: its meshes, joints, cells and
    /// lenses in housing space. When present it is what the stage draws, and
    /// the QLC+ fields below are only what search and the patch page read.
    /// Never serialized: it is rebuilt from the spec, not stored.
    #[serde(skip)]
    pub built: Option<std::sync::Arc<luma_fixture::Built>>,
    /// QLC+ fixture type; selects the mesh and the fallback cone.
    #[serde(rename = "Type")]
    pub kind: String,
    /// Patchable modes; the head count comes from the patched one.
    #[serde(rename = "Mode")]
    pub modes: Vec<Mode>,
    /// Physical block: dimensions, pixel layout, lens.
    #[serde(rename = "Physical")]
    pub physical: Option<Physical>,
}

/// A patchable mode. Only its head count matters here — the visualizer patches
/// DMX, not pixels.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Mode {
    /// Mode name, matched against `Fixture::mode_name`.
    #[serde(rename = "@Name")]
    pub name: String,
    /// Heads, kept opaque: only the length is read.
    #[serde(rename = "Head", default)]
    pub heads: Vec<serde_json::Value>,
}

/// QLC+ `Physical`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Physical {
    /// Housing size.
    #[serde(rename = "Dimensions")]
    pub dimensions: Option<Dimensions>,
    /// Pixel grid, for procedural bars and matrices.
    #[serde(rename = "Layout")]
    pub layout: Option<Layout>,
    /// Lens block — the source of cone geometry.
    #[serde(rename = "Lens")]
    pub lens: Option<Lens>,
    /// Light source — the source of the fixture's output
    /// (`luminaire::output_lumens`).
    #[serde(rename = "Bulb", default, skip_serializing_if = "Option::is_none")]
    pub bulb: Option<Bulb>,
}

/// QLC+ `Bulb`, as far as light output goes.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Bulb {
    /// The lamp's rating in lumens, before the optics. Zero means unknown.
    #[serde(rename = "@Lumens", default, skip_serializing_if = "Option::is_none")]
    pub lumens: Option<f32>,
    /// Measured lumens out of the front of the whole fixture, from a
    /// datasheet. QLC+ has no such attribute; a library fixture carries it,
    /// and when present it wins over the lamp rating.
    #[serde(
        rename = "@OutputLumens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_lumens: Option<f32>,
}

/// Millimetres, as QLC+ writes them.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Dimensions {
    /// Width, mm.
    #[serde(rename = "@Width")]
    pub width: f32,
    /// Height, mm.
    #[serde(rename = "@Height")]
    pub height: f32,
    /// Depth, mm.
    #[serde(rename = "@Depth")]
    pub depth: f32,
}

/// Pixel grid of a procedural fixture.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Layout {
    /// Pixels per row.
    #[serde(rename = "@Width")]
    pub width: u32,
    /// Rows.
    #[serde(rename = "@Height")]
    pub height: u32,
}

/// Lens block. A fixed lens repeats one value; a zoom lens gives a range and,
/// with no zoom channel in the state model, the renderer sits at mid-zoom.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Lens {
    /// Narrow end of the beam angle, degrees. Zero means "unknown".
    #[serde(rename = "@DegreesMin")]
    pub degrees_min: f32,
    /// Wide end of the beam angle, degrees. Zero means "unknown".
    #[serde(rename = "@DegreesMax")]
    pub degrees_max: f32,
    /// Measured front-lens radius in metres, from a spec sheet. QLC+ has no
    /// such attribute; when present it wins over the per-class stand-in in
    /// [`crate::luminaire::lens_for`].
    #[serde(rename = "@RadiusM", default, skip_serializing_if = "Option::is_none")]
    pub radius_m: Option<f32>,
    /// Measured distance in metres from the head's tilt pivot forward along
    /// the beam axis to the lens centre (GDTF's `Beam` position). When present
    /// it wins over the head mesh's front face in `frame::lens_centre`.
    #[serde(rename = "@OffsetM", default, skip_serializing_if = "Option::is_none")]
    pub offset_m: Option<f32>,
}

impl Definition {
    /// Heads in the named mode, or zero when the mode is absent.
    #[must_use]
    pub fn head_count(&self, mode_name: &str) -> usize {
        self.modes
            .iter()
            .find(|m| m.name == mode_name)
            .map_or(0, |m| m.heads.len())
    }

    /// Whether this is a laser projector: a box that scans a beam through
    /// its aperture, with no heads and no DMX.
    #[must_use]
    pub fn is_laser_projector(&self) -> bool {
        self.kind == LASER_PROJECTOR
    }

    /// How far ahead of its origin, along the mount normal, a laser
    /// projector's aperture is: the middle of its front face, just proud of
    /// it, where its glass is. The beam leaves from here, in the picture
    /// and in the solve.
    #[must_use]
    pub fn aperture_m(&self) -> f32 {
        self.dimensions_m()[2] / 2.0 + 0.006
    }

    /// Physical size in metres, defaulting each missing axis to 300 mm the way
    /// `extractPhysicalDimensions` does.
    #[must_use]
    pub fn dimensions_m(&self) -> [f32; 3] {
        let d = self.physical.as_ref().and_then(|p| p.dimensions.as_ref());
        let axis = |v: f32| if v > 0.0 { v / 1000.0 } else { 0.3 };
        d.map_or([0.3; 3], |d| [axis(d.width), axis(d.height), axis(d.depth)])
    }
}
