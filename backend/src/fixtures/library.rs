//! The Luma fixture library: `luma-fixture/0` specs, shipped unpacked under
//! `resources/fixture-library/<version>/` as `index.json` plus
//! `fixtures/<id>.json`.
//!
//! A patch row names a library fixture as `lib:<id>` in `fixture_path` (and
//! its venue node's `catalog_ref`), with the library's own mode name. The spec
//! is read, built ([`luma_fixture::build`]) and lowered into a
//! [`FixtureDefinition`] once per process by [`crate::fixtures::definition`],
//! so every consumer of the QLC+ seam — DMX, roles, patch, aim, the stage —
//! reads a library fixture through the same type.
//!
//! The lowering keeps what the QLC+ shape cannot say beside it rather than
//! squeezing it in: each channel carries the [`Param`] it is a byte of
//! ([`Lowered`]), and the definition carries the build and the travel
//! ([`Source`]). `docs/design/fixture-library.md` lists what the engine still
//! does not act on.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use luma_fixture::control::{Attr, Control, Param, Range, Resolved, State};
use luma_fixture::{Built, Fixture, JointKind};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::fixtures::light::{ModeLight, Slot};
use crate::models::fixtures::{
    Bulb, Capability, Channel, ChannelColour, ChannelType, Dimensions, FixtureDefinition, Head,
    Lens, Mode, ModeChannel, Physical,
};

/// What a library fixture's `fixture_path` starts with.
pub const PREFIX: &str = "lib:";

/// The library id a `fixture_path` names, if it names one.
#[must_use]
pub fn id(fixture_path: &str) -> Option<&str> {
    fixture_path.strip_prefix(PREFIX)
}

/// The `fixture_path` a patch row stores for library fixture `id`.
#[must_use]
pub fn fixture_path(id: &str) -> String {
    format!("{PREFIX}{id}")
}

/// What a lowered definition knows that its QLC+ fields cannot carry.
#[derive(Debug)]
pub struct Source {
    pub id: String,
    /// The spec's build, in housing space; the stage draws this.
    pub built: Arc<Built>,
    /// Pan and tilt travel, `[lo, hi]` degrees from home.
    pub travel: [[f32; 2]; 2],
    /// Each mode's light, by mode name.
    pub light: HashMap<String, Arc<ModeLight>>,
}

/// One DMX slot of a lowered mode: byte `rank` of `param`.
#[derive(Debug, Clone, PartialEq)]
pub struct Lowered {
    pub param: Arc<Param>,
    /// Which of the mode's params it is: its place in [`ModeLight`]'s values.
    pub index: usize,
    pub rank: u8,
}

impl Lowered {
    /// Whether the engine drives this byte from a primitive, rather than
    /// holding it at home.
    #[must_use]
    pub fn driven(&self) -> bool {
        if self.rank > 0 {
            return matches!(self.param.attr, Attr::Pan | Attr::Tilt);
        }
        match self.param.attr {
            Attr::Dimmer | Attr::Shutter | Attr::Pan | Attr::Tilt => true,
            Attr::ColourWheel | Attr::ColourMacro => !self.param.ranges.is_empty(),
            _ => self.channel_colour() != ChannelColour::None,
        }
    }

    /// The byte this slot rests at.
    #[must_use]
    pub fn home(&self) -> u8 {
        self.param.byte(self.param.home, self.rank)
    }

    /// The QLC+ class the rest of Luma reads it as. A finer byte is nothing
    /// but a byte, except a pan or tilt's.
    #[must_use]
    pub fn channel_type(&self) -> ChannelType {
        if self.rank > 0 && !matches!(self.param.attr, Attr::Pan | Attr::Tilt) {
            return ChannelType::Nothing;
        }
        match self.param.attr {
            Attr::Dimmer
            | Attr::Red
            | Attr::Green
            | Attr::Blue
            | Attr::White
            | Attr::Amber
            | Attr::Lime
            | Attr::Uv
            | Attr::WarmWhite
            | Attr::CoolWhite
            | Attr::CmyCyan
            | Attr::CmyMagenta
            | Attr::CmyYellow => ChannelType::Intensity,
            Attr::Cyan
            | Attr::Indigo
            | Attr::Mint
            | Attr::Orange
            | Attr::Cct
            | Attr::Tint
            | Attr::Hue
            | Attr::Saturation
            | Attr::ColourMacro
            | Attr::ColourWheel => ChannelType::Colour,
            Attr::Shutter => ChannelType::Shutter,
            Attr::Pan => ChannelType::Pan,
            Attr::Tilt => ChannelType::Tilt,
            Attr::PositionSpeed => ChannelType::Speed,
            Attr::Zoom | Attr::Focus | Attr::Iris | Attr::Frost | Attr::Framing => {
                ChannelType::Beam
            }
            Attr::Prism | Attr::PrismRot => ChannelType::Prism,
            Attr::GoboWheel | Attr::GoboRot => ChannelType::Gobo,
            Attr::ShutterFx
            | Attr::PanSpin
            | Attr::TiltSpin
            | Attr::FxWheel
            | Attr::FxMacro
            | Attr::FxSpeed => ChannelType::Effect,
            Attr::Control => ChannelType::Maintenance,
            Attr::None => ChannelType::Nothing,
            Attr::Other => ChannelType::Unknown,
        }
    }

    /// Which of the engine's mixing primaries it is. Warm and cool white are
    /// both white; an additive cyan, indigo, mint or orange is a colour the
    /// mixer does not make, so none (and it rests at home).
    #[must_use]
    pub fn channel_colour(&self) -> ChannelColour {
        if self.rank > 0 {
            return ChannelColour::None;
        }
        match self.param.attr {
            Attr::Red => ChannelColour::Red,
            Attr::Green => ChannelColour::Green,
            Attr::Blue => ChannelColour::Blue,
            Attr::White | Attr::WarmWhite | Attr::CoolWhite => ChannelColour::White,
            Attr::Amber => ChannelColour::Amber,
            Attr::Lime => ChannelColour::Lime,
            Attr::Uv => ChannelColour::UV,
            Attr::CmyCyan => ChannelColour::Cyan,
            Attr::CmyMagenta => ChannelColour::Magenta,
            Attr::CmyYellow => ChannelColour::Yellow,
            _ => ChannelColour::None,
        }
    }

    /// The byte for a head at `degrees` of pan or tilt, through the param's
    /// degree ranges; past the travel, the nearest end.
    #[must_use]
    pub fn position(&self, degrees: f32) -> u8 {
        let p = &self.param;
        let deg = f64::from(degrees);
        let value = p
            .ranges
            .iter()
            .find(|r| r.covers("deg", deg))
            .map(|r| p.through(r, deg))
            .or_else(|| {
                p.ranges
                    .iter()
                    .filter(|r| r.unit.as_deref() == Some("deg"))
                    .filter_map(|r| r.phys.map(|phys| (r, phys)))
                    .flat_map(|(r, [a, b])| [(r, a), (r, b)])
                    .min_by(|x, y| (x.1 - deg).abs().total_cmp(&(y.1 - deg).abs()))
                    .map(|(r, end)| p.through(r, end))
            })
            .unwrap_or(p.home);
        p.byte(value, self.rank)
    }

    /// The byte that opens the shutter: its `open` range's middle, else home.
    #[must_use]
    pub fn open(&self) -> u8 {
        let p = &self.param;
        p.byte(
            p.state(State::Open).map_or(p.home, |r| p.middle(r)),
            self.rank,
        )
    }

    /// The byte that strobes at `hz`: through the `strobe` range's `phys`
    /// when it is in Hz, across the range by `strobe` (0–1) when it is not,
    /// and across the whole channel when it has no ranges. A shutter that
    /// cannot strobe stays open.
    #[must_use]
    pub fn strobe(&self, strobe: f32, hz: f64) -> u8 {
        let p = &self.param;
        let value = match p.state(State::Strobe) {
            Some(r) if r.unit.as_deref() == Some("Hz") && r.phys.is_some() => {
                let [a, b] = r.phys.unwrap_or_default();
                p.through(r, hz.clamp(a.min(b), a.max(b)))
            }
            Some(r) => {
                let [lo, hi] = p.widen(r.dmx);
                lo + (f64::from(strobe.clamp(0.0, 1.0)) * f64::from(hi - lo)).round() as u32
            }
            None if p.ranges.is_empty() => {
                (f64::from(strobe.clamp(0.0, 1.0)) * f64::from(p.max())).round() as u32
            }
            None => return self.open(),
        };
        p.byte(value, self.rank)
    }
}

// ---------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------

/// `index.json`: one entry per shipped fixture.
#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    pub version: String,
    pub fixtures: Vec<Entry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub manufacturer: String,
    pub model: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub cells: u32,
    pub modes: Vec<EntryMode>,
    pub quality: Quality,
    #[serde(default)]
    pub aliases: Option<Aliases>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct EntryMode {
    pub name: String,
    pub footprint: usize,
}

/// How far a fixture has been checked: its geometry `reviewed` or `verified`,
/// the reviewer's verdict, and per mode how its layout was settled (`manual`,
/// `agreed`, `majority_only`, `single`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Quality {
    pub geometry: String,
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub modes: BTreeMap<String, String>,
}

/// What the fixture was called in other libraries.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Aliases {
    #[serde(default)]
    pub qlc_path: Option<String>,
    #[serde(default)]
    pub qlc_modes: BTreeMap<String, String>,
}

/// The index of the library at `root`, read at most once per process.
///
/// # Errors
/// The file is missing or is not an index.
pub fn index(root: &Path) -> Result<Arc<Index>, String> {
    static CACHE: Lazy<Mutex<HashMap<PathBuf, Arc<Index>>>> = Lazy::new(Default::default);
    let cache = || CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(hit) = cache().get(root) {
        return Ok(hit.clone());
    }
    let path = root.join("index.json");
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let index: Arc<Index> =
        Arc::new(serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?);
    cache().insert(root.to_path_buf(), index.clone());
    Ok(index)
}

/// Where fixture `id`'s spec is under the library at `root`.
#[must_use]
pub fn spec_path(root: &Path, id: &str) -> PathBuf {
    root.join("fixtures").join(format!("{id}.json"))
}

// ---------------------------------------------------------------------------
// Loading and lowering
// ---------------------------------------------------------------------------

/// The spec at `path`, built and lowered.
///
/// # Errors
/// The file cannot be read, is not a `luma-fixture/0` spec, does not build,
/// or has a mode that does not resolve.
pub fn load(path: &Path) -> Result<FixtureDefinition, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let fixture = Fixture::from_json(&text).map_err(|e| e.to_string())?;
    let built = luma_fixture::build(&fixture).map_err(|e| e.to_string())?;
    lower(&fixture, built)
}

/// A spec in the shape the QLC+ consumers read.
///
/// # Errors
/// A mode does not resolve.
pub fn lower(fixture: &Fixture, built: Built) -> Result<FixtureDefinition, String> {
    let control = match &fixture.control {
        Some(value) => Control::from_value(value).map_err(|e| format!("control: {e}"))?,
        None => Control {
            native_cct_k: None,
            wheels: BTreeMap::new(),
            tables: BTreeMap::new(),
            modes: Vec::new(),
        },
    };
    let cell_count = built.cells.len();
    let travel = travel(&built, &control);
    let mut channels = Vec::new();
    let mut modes = Vec::new();
    let mut light = HashMap::new();
    for mode in &control.modes {
        let resolved = control.resolve(&mode.name).map_err(|e| e.to_string())?;
        let (lowered, mode_light) = lower_mode(
            &control,
            &built,
            &resolved,
            cell_count,
            travel,
            &mut channels,
        );
        light.insert(resolved.name.clone(), Arc::new(mode_light));
        modes.push(lowered);
    }
    let (lo, hi) = built.bounds().unwrap_or_default();
    let size = (hi - lo) * 1000.0;
    let meta = &fixture.meta;
    Ok(FixtureDefinition {
        manufacturer: meta.manufacturer.clone().unwrap_or_default(),
        model: meta.model.clone().unwrap_or_else(|| meta.id.clone()),
        type_: qlc_type(meta.category.as_deref(), cell_count).to_string(),
        channels,
        modes,
        physical: Some(Physical {
            dimensions: Some(Dimensions {
                weight: 0.0,
                width: size.x,
                height: size.y,
                depth: size.z,
            }),
            layout: None,
            bulb: output_lumens(fixture).map(|lumens| Bulb {
                type_: None,
                lumens: None,
                colour_temperature: None,
                output_lumens: Some(lumens),
            }),
            lens: beam_degrees(fixture).map(|[lo, hi]| Lens {
                name: None,
                degrees_min: Some(lo),
                degrees_max: Some(hi),
            }),
            focus: None,
            technical: None,
        }),
        library: Some(Arc::new(Source {
            id: meta.id.clone(),
            built: Arc::new(built),
            travel,
            light,
        })),
    })
}

/// One resolved mode as a QLC+ mode, its channels appended to `channels`.
///
/// Every slot is its own channel, named for the mode and offset so a name
/// never stands for two different bytes. A fixture with more than one cell
/// gets one head per canonical cell, whatever the mode addresses, so
/// `fixture:<cell>` means the same in every mode (CONTROL.md §5); a channel
/// over a range of cells sits in the range's first head and reads their mean.
fn lower_mode(
    control: &Control,
    built: &Built,
    resolved: &Resolved,
    cell_count: usize,
    travel: [[f32; 2]; 2],
    channels: &mut Vec<Channel>,
) -> (Mode, ModeLight) {
    let has_emitters = resolved
        .params
        .iter()
        .any(|p| p.attr != Attr::Dimmer && emitter(p.attr));
    let params: Vec<Arc<Param>> = resolved
        .params
        .iter()
        .map(|p| Arc::new(prepare(control, p, has_emitters, travel)))
        .collect();
    let mut heads = vec![Head { channels: vec![] }; if cell_count > 1 { cell_count } else { 0 }];
    let mut mode_channels = Vec::with_capacity(resolved.slots.len());
    for (offset, &(index, rank)) in resolved.slots.iter().enumerate() {
        let param = params[index].clone();
        let name = format!("{} {}: {}", resolved.name, offset + 1, label(&param, rank));
        if let (Some(cell), false) = (param.cell, heads.is_empty()) {
            let first = cell.span()[0] as usize;
            if let Some(head) = heads.get_mut(first) {
                head.channels.push(offset as u32);
            }
        }
        let capabilities = colour_capabilities(control, &param);
        channels.push(Channel {
            name: name.clone(),
            preset: None,
            group: None,
            capabilities,
            lowered: Some(Box::new(Lowered { param, index, rank })),
        });
        mode_channels.push(ModeChannel {
            number: offset as u32,
            name,
        });
    }
    let light = ModeLight::new(built, resolved, &params, |p| {
        colour_capabilities(control, p)
            .iter()
            .filter_map(|cap| {
                Some(Slot {
                    byte: cap.min,
                    rgb: crate::fixtures::light::hex(cap.res1.as_deref()?)?,
                })
            })
            .collect()
    });
    (
        Mode {
            name: resolved.name.clone(),
            channels: mode_channels,
            heads,
        },
        light,
    )
}

/// `p` as the engine reads it. A colour macro on a fixture that mixes its
/// own colour stays at `off` (its ranges dropped, so nothing picks one); a
/// pan or tilt without a degree mapping gets the fixture's travel across the
/// whole channel.
fn prepare(control: &Control, p: &Param, has_emitters: bool, travel: [[f32; 2]; 2]) -> Param {
    let mut p = p.clone();
    let axis = match p.attr {
        Attr::Pan => Some(0),
        Attr::Tilt => Some(1),
        _ => None,
    };
    if let Some(axis) = axis {
        if !p.ranges.iter().any(|r| r.unit.as_deref() == Some("deg")) {
            p.ranges = vec![Range {
                dmx: [0, 255],
                state: None,
                phys: Some(travel[axis].map(f64::from)),
                unit: Some("deg".into()),
                slot: None,
                rgb: None,
                n: None,
            }];
        }
    }
    let colours = |p: &Param| !colour_capabilities(control, p).is_empty();
    if (p.attr == Attr::ColourMacro && has_emitters)
        || (p.attr == Attr::ColourWheel && !colours(&p))
    {
        p.ranges.clear();
    }
    p
}

fn emitter(attr: Attr) -> bool {
    matches!(
        attr,
        Attr::Red
            | Attr::Green
            | Attr::Blue
            | Attr::White
            | Attr::Amber
            | Attr::Lime
            | Attr::Cyan
            | Attr::Indigo
            | Attr::Mint
            | Attr::Orange
            | Attr::Uv
            | Attr::WarmWhite
            | Attr::CoolWhite
            | Attr::CmyCyan
            | Attr::CmyMagenta
            | Attr::CmyYellow
    )
}

/// A colour wheel's or colour preset's ranges as QLC+ capabilities with a
/// colour, which is what the engine's nearest-colour pick reads. Splits,
/// rotations and effects carry no colour and are left out.
fn colour_capabilities(control: &Control, p: &Param) -> Vec<Capability> {
    let rgb = |r: &Range| -> Option<String> {
        match p.attr {
            Attr::ColourMacro => r.rgb.clone(),
            Attr::ColourWheel => {
                let slot = match (r.state, r.slot) {
                    (Some(State::Slot), Some(luma_fixture::control::SlotRef::One(s))) => s,
                    _ => return None,
                };
                let wheel = control.wheels.get(p.wheel.as_deref()?)?;
                let slot = wheel.get(slot as usize)?;
                match slot.kind.as_str() {
                    "open" => Some("#ffffff".into()),
                    "colour" => slot.rgb.clone(),
                    _ => None,
                }
            }
            _ => None,
        }
    };
    p.ranges
        .iter()
        .filter_map(|r| {
            let colour = rgb(r)?;
            Some(Capability {
                min: r.dmx[0],
                max: r.dmx[1],
                preset: None,
                res1: Some(colour),
                res2: None,
                res: None,
                color: None,
                color_2: None,
                label: String::new(),
            })
        })
        .collect()
}

/// A slot's name as a person reads it: `colour/red cell 3`, `position/pan fine`.
fn label(p: &Param, rank: u8) -> String {
    let mut out = p.attr.path();
    if let Some(n) = p.n {
        out.push_str(&format!(" {n}"));
    }
    match p.cell.map(luma_fixture::control::CellRef::span) {
        Some([a, b]) if a == b => out.push_str(&format!(" cell {a}")),
        Some([a, b]) => out.push_str(&format!(" cells {a}–{b}")),
        None => {}
    }
    match rank {
        0 => {}
        1 => out.push_str(" fine"),
        _ => out.push_str(" ultra"),
    }
    out
}

/// Pan and tilt travel: the rig's joint range, else the first degree mapping
/// a mode gives, else a common mover's 540° by 270°.
fn travel(built: &Built, control: &Control) -> [[f32; 2]; 2] {
    let joints = [JointKind::Pan, JointKind::Tilt];
    let attrs = [Attr::Pan, Attr::Tilt];
    let fallback = [[-270.0, 270.0], [-135.0, 135.0]];
    std::array::from_fn(|axis| {
        luma_scene::library::own_joint(built, joints[axis])
            .and_then(|j| built.joints[j].range)
            .or_else(|| {
                control.modes.iter().find_map(|m| {
                    let resolved = control.resolve(&m.name).ok()?;
                    resolved
                        .params
                        .iter()
                        .filter(|p| p.attr == attrs[axis])
                        .flat_map(|p| p.ranges.iter())
                        .find(|r| r.unit.as_deref() == Some("deg"))
                        .and_then(|r| r.phys)
                        .map(|[a, b]| [a.min(b) as f32, a.max(b) as f32])
                })
            })
            .unwrap_or(fallback[axis])
    })
}

/// The fixture's output from `optics`, lumens, when its datasheet gave one.
fn output_lumens(fixture: &Fixture) -> Option<f32> {
    let lumens = fixture.optics.as_ref()?.get("lumens")?.as_f64()? as f32;
    (lumens.is_finite() && lumens > 0.0).then_some(lumens)
}

/// The beam angle range from `optics`: its zoom, or its one beam angle.
fn beam_degrees(fixture: &Fixture) -> Option<[f32; 2]> {
    let optics = fixture.optics.as_ref()?;
    if let Some([a, b]) = optics
        .get("zoom_deg")
        .and_then(|z| serde_json::from_value::<[f32; 2]>(z.clone()).ok())
    {
        return Some([a.min(b), a.max(b)]);
    }
    let beam = optics.get("beam_deg")?.as_f64()? as f32;
    Some([beam, beam])
}

/// The QLC+ `Type` a library category reads as, which is what role
/// derivation and the stage's lens classes key on.
fn qlc_type(category: Option<&str>, cells: usize) -> &'static str {
    let category = category.unwrap_or("");
    if category.starts_with("moving_head") {
        return "Moving Head";
    }
    match category {
        "scanner" => "Scanner",
        "laser" => "Laser",
        "strobe_or_blinder" | "blinder" => "Strobe",
        "effect_light" | "static_led_effect" => "Effect",
        "pixel_bar"
        | "led_bar"
        | "matrix_panel"
        | "led_panel_or_tile"
        | "led_panel"
        | "panel"
        | "multi_head_array"
        | "halogen_decorative_array"
            if cells > 1 =>
        {
            "LED Bar (Pixels)"
        }
        _ => "Color Changer",
    }
}

#[cfg(test)]
mod tests;
