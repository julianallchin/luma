//! A spec's `control` section, typed, and one mode of it resolved into the
//! values it drives.
//!
//! The format is the fixtures repository's `docs/CONTROL.md`; section numbers
//! below are that document's. [`Control::resolve`] does what §16.1 asks of a
//! loader, once per mode: expand `repeat` blocks, look up range tables, pair
//! byte ranks into one [`Param`] per value (§4) and work out each value's home
//! (§7). The DMX engine reads [`Resolved`] and never the raw entries.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The `control` section of one fixture.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Control {
    /// The CCT of full white (§8).
    #[serde(default)]
    pub native_cct_k: Option<f64>,
    #[serde(default)]
    pub wheels: BTreeMap<String, Vec<WheelSlot>>,
    #[serde(default)]
    pub tables: BTreeMap<String, Vec<Range>>,
    pub modes: Vec<Mode>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Mode {
    /// Unique within the fixture; what a venue row stores.
    pub name: String,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    /// DMX slots the mode uses.
    pub footprint: usize,
    pub channels: Vec<Entry>,
}

/// One entry of a mode's channel list: a slot, or a block repeated per cell.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Entry {
    Repeat { repeat: Repeat },
    Channel(Channel),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Repeat {
    pub cells: Targets,
    pub channels: Vec<Channel>,
}

/// Where a `repeat` block writes its channels, in DMX order (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Targets {
    /// Every cell from `from` to `to`, counting down when `from > to`, in
    /// blocks of `size`.
    Span {
        from: u32,
        to: u32,
        #[serde(default)]
        size: Option<u32>,
    },
    List(Vec<CellRef>),
}

/// A channel's cell: one canonical cell, or an inclusive range of them (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(untagged)]
pub enum CellRef {
    One(u32),
    Span([u32; 2]),
}

impl CellRef {
    /// The first and last cell it covers.
    #[must_use]
    pub fn span(self) -> [u32; 2] {
        match self {
            Self::One(c) => [c, c],
            Self::Span([a, b]) => [a.min(b), a.max(b)],
        }
    }
}

/// One DMX slot as the spec writes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Channel {
    pub attr: Attr,
    /// Byte significance: 0 coarse, 1 fine, 2 ultra (§4).
    #[serde(default)]
    pub rank: u8,
    #[serde(default)]
    pub cell: Option<CellRef>,
    #[serde(default)]
    pub wheel: Option<String>,
    /// Instance number of an attribute that repeats on one cell and wheel.
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub ranges: Option<Ranges>,
    /// The home override, in coarse units (§7).
    #[serde(default)]
    pub home: Option<f64>,
    #[serde(default)]
    pub fade: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Ranges {
    /// The name of one of [`Control::tables`].
    Table(String),
    Inline(Vec<Range>),
}

/// One span of a channel's DMX values (§6). `dmx` is inclusive and in coarse
/// units whatever the channel's width.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Range {
    pub dmx: [u8; 2],
    #[serde(default)]
    pub state: Option<State>,
    /// `dmx[0]` maps to `phys[0]` and `dmx[1]` to `phys[1]`, linearly.
    #[serde(default)]
    pub phys: Option<[f64; 2]>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub slot: Option<SlotRef>,
    /// sRGB `#rrggbb`, for a colour preset.
    #[serde(default)]
    pub rgb: Option<String>,
    #[serde(default)]
    pub n: Option<Value>,
}

impl Range {
    /// Whether `phys` is in `unit` and covers `value`.
    #[must_use]
    pub fn covers(&self, unit: &str, value: f64) -> bool {
        self.unit.as_deref() == Some(unit)
            && self
                .phys
                .is_some_and(|[a, b]| (a.min(b) - 1e-9..=a.max(b) + 1e-9).contains(&value))
    }
}

/// A wheel slot, or the two a `split` sits between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum SlotRef {
    One(u32),
    Two([u32; 2]),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WheelSlot {
    /// `open`, `closed`, `frost`, `anim`, `colour`, `gobo`, `prism`.
    pub kind: String,
    #[serde(default)]
    pub rgb: Option<String>,
    /// A CTO/CTB filter's temperature.
    #[serde(default)]
    pub k: Option<f64>,
    #[serde(default)]
    pub gobo: Option<String>,
    #[serde(default)]
    pub facets: Option<f64>,
}

/// The closed attribute vocabulary (§3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Attr {
    #[serde(rename = "dimmer")]
    Dimmer,
    #[serde(rename = "shutter")]
    Shutter,
    #[serde(rename = "shutter/fx")]
    ShutterFx,
    #[serde(rename = "colour/red")]
    Red,
    #[serde(rename = "colour/green")]
    Green,
    #[serde(rename = "colour/blue")]
    Blue,
    #[serde(rename = "colour/white")]
    White,
    #[serde(rename = "colour/amber")]
    Amber,
    #[serde(rename = "colour/lime")]
    Lime,
    #[serde(rename = "colour/cyan")]
    Cyan,
    #[serde(rename = "colour/indigo")]
    Indigo,
    #[serde(rename = "colour/mint")]
    Mint,
    #[serde(rename = "colour/orange")]
    Orange,
    #[serde(rename = "colour/uv")]
    Uv,
    #[serde(rename = "colour/warm_white")]
    WarmWhite,
    #[serde(rename = "colour/cool_white")]
    CoolWhite,
    #[serde(rename = "cmy/cyan")]
    CmyCyan,
    #[serde(rename = "cmy/magenta")]
    CmyMagenta,
    #[serde(rename = "cmy/yellow")]
    CmyYellow,
    #[serde(rename = "colour/cct")]
    Cct,
    #[serde(rename = "colour/tint")]
    Tint,
    #[serde(rename = "colour/hue")]
    Hue,
    #[serde(rename = "colour/saturation")]
    Saturation,
    #[serde(rename = "colour/macro")]
    ColourMacro,
    #[serde(rename = "colour/wheel")]
    ColourWheel,
    #[serde(rename = "position/pan")]
    Pan,
    #[serde(rename = "position/tilt")]
    Tilt,
    #[serde(rename = "position/pan_spin")]
    PanSpin,
    #[serde(rename = "position/tilt_spin")]
    TiltSpin,
    #[serde(rename = "position/speed")]
    PositionSpeed,
    #[serde(rename = "beam/zoom")]
    Zoom,
    #[serde(rename = "beam/focus")]
    Focus,
    #[serde(rename = "beam/iris")]
    Iris,
    #[serde(rename = "beam/frost")]
    Frost,
    #[serde(rename = "beam/prism")]
    Prism,
    #[serde(rename = "beam/prism_rot")]
    PrismRot,
    #[serde(rename = "beam/framing")]
    Framing,
    #[serde(rename = "gobo/wheel")]
    GoboWheel,
    #[serde(rename = "gobo/rot")]
    GoboRot,
    #[serde(rename = "fx/wheel")]
    FxWheel,
    #[serde(rename = "fx/macro")]
    FxMacro,
    #[serde(rename = "fx/speed")]
    FxSpeed,
    #[serde(rename = "control")]
    Control,
    #[serde(rename = "none")]
    None,
    #[serde(rename = "other")]
    Other,
}

impl Attr {
    /// The path the spec writes, `colour/red` and so on.
    #[must_use]
    pub fn path(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// Pan or tilt: a head's position, mapped through degrees.
    #[must_use]
    pub fn is_position(self) -> bool {
        matches!(self, Self::Pan | Self::Tilt)
    }

    /// What the attribute rests at when nothing drives it (§7).
    fn policy(self) -> Home {
        match self {
            Self::Dimmer
            | Self::Red
            | Self::Green
            | Self::Blue
            | Self::White
            | Self::Amber
            | Self::Lime
            | Self::Cyan
            | Self::Indigo
            | Self::Mint
            | Self::Orange
            | Self::Uv
            | Self::WarmWhite
            | Self::CoolWhite
            | Self::CmyCyan
            | Self::CmyMagenta
            | Self::CmyYellow
            | Self::Saturation
            | Self::Frost => Home::Phys("%", 0.0),
            Self::Shutter | Self::Iris => Home::State(State::Open, None),
            Self::Cct => Home::Native,
            Self::Tint => Home::Phys("", 0.0),
            Self::Hue | Self::Pan | Self::Tilt => Home::Phys("deg", 0.0),
            Self::ColourMacro | Self::FxMacro => Home::State(State::Off, None),
            Self::ColourWheel | Self::GoboWheel | Self::Prism | Self::FxWheel => {
                Home::State(State::Slot, Some(0))
            }
            Self::PanSpin | Self::TiltSpin | Self::GoboRot | Self::PrismRot => {
                Home::State(State::Stop, None)
            }
            Self::Zoom | Self::Focus => Home::Phys("%", 50.0),
            Self::ShutterFx
            | Self::PositionSpeed
            | Self::FxSpeed
            | Self::Control
            | Self::None
            | Self::Other
            | Self::Framing => Home::Dmx0,
        }
    }
}

/// The named states (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Open,
    Closed,
    Strobe,
    Pulse,
    Random,
    Cw,
    Ccw,
    Stop,
    Index,
    Slot,
    Split,
    Scroll,
    Shake,
    Off,
    Macro,
    Sound,
    Reset,
    LampOn,
    LampOff,
}

/// A policy home: a state's range, a physical value, the native CCT, or 0.
enum Home {
    State(State, Option<u32>),
    Phys(&'static str, f64),
    Native,
    Dmx0,
}

/// One mode, resolved: one [`Param`] per value it drives, and which param
/// and byte every DMX slot is.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub name: String,
    pub footprint: usize,
    pub params: Vec<Param>,
    /// `slots[offset] = (param index, rank)`, one per DMX slot.
    pub slots: Vec<(usize, u8)>,
}

/// One value a mode drives: an attribute on a cell (or the whole fixture),
/// over one or more bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub attr: Attr,
    /// `None` = the fixture primitive.
    pub cell: Option<CellRef>,
    pub wheel: Option<String>,
    pub n: Option<u32>,
    /// The DMX offset of each byte, most significant first.
    pub bytes: Vec<usize>,
    /// The channel's ranges, its table looked up. Empty = plain 0–100 %.
    pub ranges: Vec<Range>,
    /// Where it rests, at the param's full width ([`Self::max`]).
    pub home: u32,
}

impl Param {
    /// The largest value its bytes hold: 255, 65535, ...
    #[must_use]
    pub fn max(&self) -> u32 {
        let bits = 8 * self.bytes.len().clamp(1, 4) as u32;
        u32::try_from((1_u64 << bits) - 1).unwrap_or(u32::MAX)
    }

    /// A coarse `[lo, hi]` at full width: `lo · 256^K` to
    /// `hi · 256^K + 256^K − 1` (§4).
    #[must_use]
    pub fn widen(&self, dmx: [u8; 2]) -> [u32; 2] {
        let step = (self.max() + 1) / 256;
        let step = step.max(1);
        [
            u32::from(dmx[0]) * step,
            u32::from(dmx[1]) * step + (step - 1),
        ]
    }

    /// The value at full width that `range` maps `phys` to, clamped to the
    /// range.
    #[must_use]
    pub fn through(&self, range: &Range, phys: f64) -> u32 {
        let [lo, hi] = self.widen(range.dmx);
        let Some([a, b]) = range.phys else {
            return lo;
        };
        let t = if (b - a).abs() < f64::EPSILON {
            0.0
        } else {
            ((phys - a) / (b - a)).clamp(0.0, 1.0)
        };
        (f64::from(lo) + t * f64::from(hi - lo)).round() as u32
    }

    /// The middle of `range` at full width.
    #[must_use]
    pub fn middle(&self, range: &Range) -> u32 {
        let [lo, hi] = self.widen(range.dmx);
        lo + (hi - lo) / 2
    }

    /// The byte at `rank` of `value` (at full width).
    #[must_use]
    pub fn byte(&self, value: u32, rank: u8) -> u8 {
        let k = self.bytes.len().saturating_sub(1);
        let shift = 8 * k.saturating_sub(usize::from(rank));
        ((value >> shift) & 0xff) as u8
    }

    /// The first range in `state`.
    #[must_use]
    pub fn state(&self, state: State) -> Option<&Range> {
        self.ranges.iter().find(|r| r.state == Some(state))
    }
}

/// What pairs a finer byte with its coarse byte (§4).
type Key = (Attr, Option<CellRef>, Option<String>, Option<u32>);

/// Why a mode could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError(pub String);

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ResolveError {}

impl Control {
    /// The `control` value of a spec.
    ///
    /// # Errors
    /// The value is not a `control` section.
    pub fn from_value(value: &Value) -> Result<Self, serde_json::Error> {
        Self::deserialize(value)
    }

    /// The mode called `name`, resolved.
    ///
    /// # Errors
    /// No such mode; a table it names is missing; a fine byte has no coarse
    /// byte to pair with; or it expands to other than its footprint. The
    /// library's gate refuses modes with these faults, so a shipped spec never
    /// has one.
    pub fn resolve(&self, name: &str) -> Result<Resolved, ResolveError> {
        let mode = self
            .modes
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| ResolveError(format!("no mode {name:?}")))?;
        let flat = expand(&mode.channels);
        if flat.len() != mode.footprint {
            return Err(ResolveError(format!(
                "mode {name:?} expands to {} slots, footprint {}",
                flat.len(),
                mode.footprint
            )));
        }
        let key = |c: &Channel| -> Key { (c.attr, c.cell, c.wheel.clone(), c.n) };
        let mut params: Vec<Param> = Vec::new();
        let mut keys: Vec<Key> = Vec::new();
        let mut slots: Vec<Option<(usize, u8)>> = Vec::with_capacity(flat.len());
        for (offset, c) in flat.iter().enumerate() {
            if c.rank == 0 {
                let ranges = match &c.ranges {
                    None => Vec::new(),
                    Some(Ranges::Inline(list)) => list.clone(),
                    Some(Ranges::Table(table)) => self
                        .tables
                        .get(table)
                        .cloned()
                        .ok_or_else(|| ResolveError(format!("no table {table:?}")))?,
                };
                params.push(Param {
                    attr: c.attr,
                    cell: c.cell,
                    wheel: c.wheel.clone(),
                    n: c.n,
                    bytes: vec![offset],
                    ranges,
                    home: 0,
                });
                keys.push(key(c));
                slots.push(Some((params.len() - 1, 0)));
            } else {
                slots.push(None);
            }
        }
        // Pair each finer byte with the nearest coarse byte of its key before
        // it, else the first after (§4).
        for (offset, c) in flat.iter().enumerate() {
            if c.rank == 0 {
                continue;
            }
            let k = key(c);
            let candidates: Vec<usize> = (0..params.len()).filter(|&i| keys[i] == k).collect();
            let owner = candidates
                .iter()
                .rev()
                .find(|&&i| params[i].bytes[0] < offset)
                .or_else(|| candidates.first())
                .copied()
                .ok_or_else(|| {
                    ResolveError(format!(
                        "slot {offset}: {} rank {} has no coarse byte",
                        c.attr.path(),
                        c.rank
                    ))
                })?;
            let rank = usize::from(c.rank);
            let bytes = &mut params[owner].bytes;
            if bytes.len() != rank {
                return Err(ResolveError(format!(
                    "slot {offset}: {} rank {} out of order",
                    c.attr.path(),
                    c.rank
                )));
            }
            bytes.push(offset);
            slots[offset] = Some((owner, c.rank));
        }
        for (p, c) in params.iter_mut().zip(flat.iter().filter(|c| c.rank == 0)) {
            p.home = self.home(p, c.home);
        }
        Ok(Resolved {
            name: mode.name.clone(),
            footprint: mode.footprint,
            params,
            // Every slot is a coarse byte or was paired above (an unpaired one
            // returned), so none is dropped here.
            slots: slots.into_iter().flatten().collect(),
        })
    }

    /// Where `p` rests, at full width, in the order §7 resolves it.
    fn home(&self, p: &Param, over: Option<f64>) -> u32 {
        if let Some(coarse) = over {
            let coarse = coarse.clamp(0.0, 255.0).round() as u8;
            return p.widen([coarse, coarse])[0];
        }
        let phys = |unit: &str, value: f64| {
            p.ranges
                .iter()
                .find(|r| r.covers(unit, value))
                .map(|r| p.through(r, value))
        };
        let found = match p.attr.policy() {
            Home::State(state, slot) => p
                .ranges
                .iter()
                .find(|r| {
                    r.state == Some(state) && (slot.is_none() || r.slot == slot.map(SlotRef::One))
                })
                .map(|r| p.middle(r)),
            Home::Phys(unit, value) => phys(unit, value).or_else(|| {
                // A plain 0–100 % channel scales its percentage (§7.4).
                (p.ranges.is_empty() && unit == "%")
                    .then(|| (value / 100.0 * f64::from(p.max())).round() as u32)
            }),
            Home::Native => self.native_cct_k.and_then(|k| phys("K", k)),
            Home::Dmx0 => None,
        };
        found.unwrap_or(0)
    }
}

/// The mode's channels with every `repeat` written out: one per DMX slot.
fn expand(entries: &[Entry]) -> Vec<Channel> {
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            Entry::Channel(c) => out.push(c.clone()),
            Entry::Repeat { repeat } => {
                for target in targets(&repeat.cells) {
                    for c in &repeat.channels {
                        out.push(Channel {
                            cell: Some(target),
                            ..c.clone()
                        });
                    }
                }
            }
        }
    }
    out
}

fn targets(t: &Targets) -> Vec<CellRef> {
    match t {
        Targets::List(list) => list.clone(),
        &Targets::Span { from, to, size } => {
            let size = size.unwrap_or(1).max(1);
            let up = to >= from;
            let mut out = Vec::new();
            let mut at = i64::from(from);
            let step = if up { 1 } else { -1 };
            let end = i64::from(to);
            while if up { at <= end } else { at >= end } {
                let a = at as u32;
                out.push(if size == 1 {
                    CellRef::One(a)
                } else {
                    let b = (at + step * i64::from(size - 1)) as u32;
                    CellRef::Span([a.min(b), a.max(b)])
                });
                at += step * i64::from(size);
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(json: &str) -> Control {
        Control::from_value(&serde_json::from_str(json).unwrap()).unwrap()
    }

    #[test]
    fn repeats_expand_in_dmx_order_and_fill_the_footprint() {
        let c = control(
            r#"{"modes": [{"name": "m", "footprint": 8, "channels": [
                {"attr": "dimmer"},
                {"repeat": {"cells": {"from": 2, "to": 0}, "channels": [{"attr": "colour/red"}]}},
                {"repeat": {"cells": {"from": 0, "to": 5, "size": 3}, "channels": [{"attr": "colour/blue"}]}},
                {"repeat": {"cells": [4, [5, 6]], "channels": [{"attr": "colour/white"}]}}
            ]}]}"#,
        );
        let m = c.resolve("m").unwrap();
        let cells: Vec<Option<CellRef>> = m.params.iter().map(|p| p.cell).collect();
        assert_eq!(
            cells,
            [
                None,
                Some(CellRef::One(2)),
                Some(CellRef::One(1)),
                Some(CellRef::One(0)),
                Some(CellRef::Span([0, 2])),
                Some(CellRef::Span([3, 5])),
                Some(CellRef::One(4)),
                Some(CellRef::Span([5, 6])),
            ]
        );
        assert_eq!(m.slots.len(), m.footprint);
    }

    #[test]
    fn a_fine_byte_pairs_with_its_own_coarse_byte_even_when_interleaved() {
        let c = control(
            r#"{"modes": [{"name": "m", "footprint": 4, "channels": [
                {"attr": "position/pan"}, {"attr": "position/tilt"},
                {"attr": "position/pan", "rank": 1}, {"attr": "position/tilt", "rank": 1}
            ]}]}"#,
        );
        let m = c.resolve("m").unwrap();
        assert_eq!(m.params[0].bytes, [0, 2]);
        assert_eq!(m.params[1].bytes, [1, 3]);
        assert_eq!(m.slots, [(0, 0), (1, 0), (0, 1), (1, 1)]);
        assert_eq!(m.params[0].max(), 65535);
    }

    #[test]
    fn homes_follow_the_policy() {
        let c = control(
            r#"{"tables": {"pan": [{"dmx": [0, 255], "phys": [-270, 360], "unit": "deg"}]},
                "modes": [{"name": "m", "footprint": 6, "channels": [
                {"attr": "shutter", "ranges": [{"dmx": [0, 9], "state": "closed"}, {"dmx": [10, 20], "state": "open"}]},
                {"attr": "position/pan", "ranges": "pan"}, {"attr": "position/pan", "rank": 1},
                {"attr": "beam/zoom"}, {"attr": "dimmer"}, {"attr": "control", "home": 12}
            ]}]}"#,
        );
        let m = c.resolve("m").unwrap();
        let home = |i: usize| m.params[i].home;
        // The open range's middle, not its edge.
        assert!((10..=20).contains(&home(0)) && home(0) > 10);
        // Pan 0° on an asymmetric travel is not the middle of the channel.
        let pan = &m.params[1];
        let zero = f64::from(home(1)) / f64::from(pan.max());
        assert!((zero - 270.0 / 630.0).abs() < 1e-3, "{zero}");
        assert_eq!(pan.byte(home(1), 0), (home(1) >> 8) as u8);
        assert!((127..=128).contains(&home(2)));
        assert_eq!(home(3), 0);
        assert_eq!(home(4), 12);
    }

    #[test]
    fn unpaired_fine_bytes_and_wrong_footprints_are_refused() {
        let c = control(
            r#"{"modes": [{"name": "a", "footprint": 1, "channels": [{"attr": "dimmer", "rank": 1}]},
                          {"name": "b", "footprint": 2, "channels": [{"attr": "dimmer"}]}]}"#,
        );
        assert!(c.resolve("a").is_err());
        assert!(c.resolve("b").is_err());
        assert!(c.resolve("c").is_err());
    }
}
