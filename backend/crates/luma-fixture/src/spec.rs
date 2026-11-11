//! The `luma-fixture/0` spec, typed.
//!
//! Field names and meanings are the fixtures repository's `docs/FORMAT.md`;
//! this module adds nothing to them. Geometry is typed down to the leaves.
//! `control`, `optics` and the provenance under `meta` are kept as JSON values:
//! the builder reads only `optics.type`, and keeping them whole is what makes
//! them round-trip exactly.
//!
//! Every number is an `f64`, counts included, because the spec writes `4` and
//! `4.0` interchangeably; the builder truncates counts the way the reference
//! builder's `int()` does. Absent and `null` mean the same thing everywhere,
//! as they do to the reference builder.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One fixture: everything in its `<id>.luma.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fixture {
    /// Always `"luma-fixture/0"`.
    pub format: String,
    pub meta: Meta,
    pub template: Template,
    pub dims: Dims,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materials: Option<BTreeMap<String, MaterialOverride>>,
    pub rig: Rig,
    pub faces: Vec<Face>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array: Option<Array>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deflector: Option<Deflector>,
    /// Derived counts the reference builder wrote; informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<Value>,
    /// Derived per-zone sources the reference builder wrote; informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<Value>,
    /// DMX modes, channels and wheels (the fixtures repository's `CONTROL.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    /// `<mfr-slug>/<model-slug>`, the file's path.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    /// One chain: mount, pan, tilt, body.
    Luminaire,
    /// `array.n` copies of the body on a carrier.
    Array,
    /// A fixed housing whose pan/tilt move an internal mirror, drum or galvo.
    Deflector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dims {
    /// Overall rest-pose box `[w, h, d]`, metres.
    pub size: [f64; 3],
}

/// An override of one entry of the colour table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emit: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metal: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rough: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rig {
    pub mount: Mount,
    pub joints: Joints,
    pub body: Body,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<Part>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mount {
    #[serde(rename = "type")]
    pub kind: MountType,
    /// 3 = tripod, 0 = foot (stand).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Base>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yoke: Option<Yoke>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clamp: Option<Clamp>,
    /// Built upside down from the top of `dims.size[1]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hang: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountType {
    Yoke2,
    Yoke1,
    Bail,
    Bracket,
    Stand,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Base {
    pub shape: BaseShape,
    pub w: f64,
    pub h: f64,
    pub d: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<Tier>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handles: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feet: Option<Feet>,
    /// U base: end block width as a fraction of `w`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseShape {
    Box,
    Disc,
    U,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tier {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chamfer: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mat: Option<MaterialSlot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Yoke {
    /// Inner span.
    pub w: f64,
    /// Arm top.
    pub y1: f64,
    /// Arm depth.
    pub d: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<YokeShape>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar_y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm: Option<Arm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hull: Option<Hull>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar: Option<YokeBar>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum YokeShape {
    Arm,
    U,
    Arch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Arm {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ArmProfile>,
    /// Thickness in x, `[bottom, top]` or one value for both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<NumOrList>,
    /// Depth in z, `[bottom, top]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bow: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_top: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cutout: Option<Cutout>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmProfile {
    Straight,
    Tapered,
    Curved,
    Cheek,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cutout {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y0: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y1: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strut: Option<f64>,
}

/// The block under the head joining the arms: a shape, or `false` for none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Hull {
    Flag(bool),
    Shape(HullShape),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HullShape {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chamfer: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YokeBar {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clamp {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Joints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<Joint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<Joint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spin: Option<Vec<Joint>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slide: Option<Vec<Joint>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Joint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive: Option<Drive>,
    /// In the joint's own frame: pan `[0,1,0]`, tilt `[1,0,0]`, spin/slide `[0,0,1]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<[f64; 3]>,
    /// `[lo, hi]` degrees (metres for a slide); absent = continuous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_head: Option<bool>,
    /// Tilt only: degrees from beam-up at rest, 90 = facing front.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Drive {
    Dmx,
    Manual,
    Fixed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Body {
    pub shape: BodyShape,
    /// Width (x).
    pub w: f64,
    /// Length along the beam.
    pub len: f64,
    /// Depth (z).
    pub d: f64,
    /// Body centre relative to the tilt axis, along the beam. A luminaire
    /// needs it; an array element does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off: Option<f64>,
    /// Tilt axis height. A luminaire needs it; an array element does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt_y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<BodyOutline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<CanProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prof_r: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taper: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ribs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens_d: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<BodyFrame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub louvres: Option<Louvres>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Shell>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pods: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sphere: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyShape {
    Can,
    Box,
    Bar,
    Panel,
    Disc,
    Dome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyOutline {
    Round,
    Square,
    Rect,
    Hex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanProfile {
    Bell,
    Taper,
    Straight,
    Stepped,
    Cylinder,
    Step2,
    Boxcan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<f64>,
}

/// A ring-with-spokes frame over the beam exit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BodyFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spokes: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoke_r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Louvres {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orient: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f64>,
}

/// A lofted head shell (FORMAT.md, "Head shell").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<ShellSection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring: Option<ShellRing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rear_cap: Option<RearCap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fins: Option<Fins>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pods: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellSection {
    pub t: f64,
    pub w: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chamfer: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellRing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bezel: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proud: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RearCap {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub len: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fins {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t0: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t1: Option<f64>,
}

/// An extra primitive (`rig.parts[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<Attach>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<PartShape>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub len: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<f64>,
    /// Loft sections `[z, rx, ry, roundness]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<[f64; 4]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bars: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rot: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mat: Option<MaterialSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attach {
    Base,
    Yoke,
    Head,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartShape {
    Box,
    Cyl,
    Sphere,
    Cone,
    Loft,
    Grille,
    Torus,
}

/// An entry of the colour table, and the material slot a mesh is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialSlot {
    Body,
    Body2,
    Metal,
    Lens,
    Diffuser,
    /// A mixing emitter (any colour without its own die colour).
    Emitter,
    /// A white-only emitter.
    EmitterW,
    EmitterR,
    EmitterG,
    EmitterB,
    EmitterA,
    EmitterUv,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Face {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<FaceSide>,
    /// A recursive face layout (preferred).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Node>,
    /// Pattern or panel zones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zones: Option<Vec<Zone>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaceSide {
    Front,
    Back,
}

/// A zone: a pattern zone (`layout` + `cell` + `optic` + `cells`) or a panel
/// zone (`region` + `cells {rows, cols}` + `emitter`). One struct for both,
/// because a face may mix them and the builder tells them apart by `emitter`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<ZoneLayout>,
    /// Pattern zone emitter outline: `circle`, `square` or `hex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<ZoneCells>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<ZoneRegion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens_d: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitter: Option<ZoneEmitter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_ids: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_white: Option<Vec<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<[f64; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<Vec<f64>>,
    /// Explicit pixel map: `[x, y, w, h]`, or `[x, y]` with `point_size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<Vec<f64>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point_size: Option<[f64; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneLayout {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stagger: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rings: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<[f64; 2]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seams: Option<Seams>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Seams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_c: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<f64>,
}

/// A pattern zone counts its DMX cells; a panel zone divides into a grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ZoneCells {
    Count(f64),
    Grid { rows: f64, cols: f64 },
}

/// A pattern zone restricts itself to a central disc; a panel zone names its
/// rectangle `[u0, v0, u1, v1]` on the face.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ZoneRegion {
    Rect([f64; 4]),
    Centre {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frac: Option<f64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneEmitter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EmitterKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colours: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch_mm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d_mm: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmitterKind {
    Led,
    Lens,
    Cob,
    Strip,
    Tube,
}

/// One node of a recursive face (FORMAT.md, "Face nodes").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<NodeOutline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<NumOrList>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<NodeLayout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<Box<Node>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<Node>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitter: Option<NodeEmitter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<CellFlag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_id: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<NodeFrame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffuser: Option<Diffuser>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<Vec<f64>>,
    /// `[about x, about y]` degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeOutline {
    Rect,
    Circle,
    Rounded,
    Hex,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeLayout {
    pub kind: LayoutKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<Dir>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stagger: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radii: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boxes: Option<Vec<[f64; 4]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pts: Option<Vec<[f64; 2]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutKind {
    Single,
    Line,
    Grid,
    Rings,
    Hex,
    Stack,
    Place,
    Points,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    X,
    Y,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeEmitter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EmitterKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colours: Option<Vec<String>>,
}

/// `true`: each instance of the node is one DMX cell; `"each"`: every emitter
/// below it is its own cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CellFlag {
    Flag(bool),
    Mode(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bezel: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recess: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Diffuser {
    Flag(bool),
    Cover {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        z: Option<f64>,
    },
}

/// A value the spec writes either as one number or as a list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NumOrList {
    Num(f64),
    List(Vec<f64>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Array {
    pub n: f64,
    pub carrier: Carrier,
    pub per_head: PerHead,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_mount: Option<Mount>,
    /// `"y"`: element beams along y (up at rest); otherwise along z (front).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beam: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells_total: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Carrier {
    pub kind: CarrierKind,
    pub w: f64,
    pub d: f64,
    pub h: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hang: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CarrierKind {
    Bar,
    Ring,
    Hub,
    Grid,
    Tower,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerHead {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<Joint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<Joint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deflector {
    pub element: DeflectorElement,
    /// Offset of the element, fixture axes `[x, y, z]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<[f64; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeflectorElement {
    Mirror,
    Drum,
    Galvo,
}

impl Fixture {
    /// Parse a spec.
    ///
    /// # Errors
    /// The text is not JSON, is not `luma-fixture/0`, or a geometry field has
    /// the wrong type or a value outside the format's vocabulary.
    pub fn from_json(text: &str) -> Result<Self, crate::Error> {
        let fixture: Self = serde_json::from_str(text).map_err(crate::Error::Parse)?;
        if fixture.format != "luma-fixture/0" {
            return Err(crate::Error::Format(fixture.format));
        }
        Ok(fixture)
    }

    /// The spec as JSON again. Geometry comes back as the same values (counts
    /// may gain a `.0`, `null` fields are left out); `control`, `optics` and
    /// `meta.provenance` come back exactly.
    ///
    /// # Panics
    /// Never: every field serializes.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a fixture always serializes")
    }

    /// `optics.type` (`spot wash beam hybrid strobe`), the one optics field the
    /// builder reads.
    #[must_use]
    pub fn optics_type(&self) -> Option<&str> {
        self.optics.as_ref()?.get("type")?.as_str()
    }
}
