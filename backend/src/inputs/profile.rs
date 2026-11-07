//! A controller's profile: plain data that says where its pads and faders
//! are and how it shows a light. No code, so an agent can write one for any
//! device.
//!
//! ```json
//! {
//!   "controls": [
//!     {"address": {"kind": "midi_note", "channel": 0, "number": 81}, "column": 0, "row": 0},
//!     {"address": {"kind": "midi_cc", "channel": 0, "number": 48}, "column": 0, "row": 8, "fader": true}
//!   ],
//!   "color": {"palette": {"0": [0, 0, 0], "5": [255, 0, 0], "21": [0, 255, 0]}},
//!   "modes": {"steady": {"channel": 0}, "blink": {"channel": 1}, "pulse": {"channel": 2}},
//!   "dim": 0.15
//! }
//! ```
//!
//! - `controls`: each pad or fader (`"fader": true`) by its MIDI address, at
//!   its place on the device's grid, `column` and `row` from the top left.
//!   Mapping a page puts the page's cell at (column, row) on the control
//!   there.
//! - `color`: how to send a color. `palette` maps each velocity the device
//!   takes to the color it shows (rgb 0–255); a light is sent as the nearest.
//!   `sysex` is a message of hex bytes for a device that takes full rgb, with
//!   `{number}` for the control's note or CC number and `{r}`, `{g}`, `{b}`
//!   for its color 0–127, such as `"F0 00 20 29 02 0C 03 03 {number} {r} {g} {b} F7"`.
//!   With no `color`, a light is echoed: the control's own note or CC, at
//!   127 lit and 0 dark.
//! - `modes`: how a palette or echoed light shows steady, pulsing and
//!   blinking: the MIDI `channel` to send on, and an `offset` added to the
//!   velocity. A mode the device lacks: a blink is drawn by turning the
//!   light on and off, a pulse is steady.
//! - `dim`: how bright a dim light is, 0–1 of a lit one (0.15 when absent).

use std::collections::BTreeMap;

use luma_patterns::color_space;
use serde::{Deserialize, Serialize};

use super::{Address, Mode};
use crate::controls::perform::{self as names, PartKind};
use crate::controls::{Job, Light, Look, Shine};
use crate::models::perform::{CellContent, LayoutPage, PerformPattern};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub controls: Vec<Place>,
    pub color: Option<ColorOut>,
    pub modes: Modes,
    pub dim: Option<f64>,
}

/// A pad or fader, where it sits on the device's grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Place {
    pub address: Address,
    pub column: u16,
    pub row: u16,
    #[serde(default)]
    pub fader: bool,
}

/// How a device takes a color.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ColorOut {
    /// Each velocity it takes, to the color it shows, rgb 0–255.
    Palette(BTreeMap<u8, [u8; 3]>),
    /// A SysEx message of hex bytes and `{number}`, `{r}`, `{g}`, `{b}`.
    Sysex(String),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Modes {
    pub steady: Option<ModeOut>,
    pub pulse: Option<ModeOut>,
    pub blink: Option<ModeOut>,
}

/// How a device is told one way of shining.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModeOut {
    /// The MIDI channel to send on, from 0; the control's own when absent.
    pub channel: Option<u8>,
    /// Added to the velocity.
    pub offset: u8,
}

/// A dim light when the profile says nothing.
const DIM: f64 = 0.15;

impl Profile {
    /// Read a profile from JSON, refusing one that cannot be drawn.
    pub fn parse(json: &str) -> Result<Self, String> {
        Self::read(
            serde_json::from_str(json).map_err(|error| format!("unreadable profile: {error}"))?,
        )
    }

    /// [`Profile::parse`], from JSON already parsed.
    pub fn read(json: serde_json::Value) -> Result<Self, String> {
        let profile: Self =
            serde_json::from_value(json).map_err(|error| format!("unreadable profile: {error}"))?;
        profile.check()?;
        Ok(profile)
    }

    /// The address of the control at (`column`, `row`).
    pub fn at(&self, column: u16, row: u16) -> Option<&Address> {
        self.controls
            .iter()
            .find(|place| (place.column, place.row) == (column, row))
            .map(|place| &place.address)
    }

    /// Why this profile cannot be used, if it cannot.
    pub fn check(&self) -> Result<(), String> {
        let midi = |address: &Address| match address {
            Address::MidiCc { channel, number } | Address::MidiNote { channel, number } => {
                *channel < 16 && *number < 128
            }
            Address::Osc { .. } => false,
        };
        if let Some(place) = self.controls.iter().find(|place| !midi(&place.address)) {
            return Err(format!(
                "a profile's control is a MIDI note or CC, channel 0–15 and number 0–127; got {:?}",
                place.address
            ));
        }
        for (index, place) in self.controls.iter().enumerate() {
            if let Some(other) = self.controls[..index].iter().find(|other| {
                other.address == place.address
                    || (other.column, other.row) == (place.column, place.row)
            }) {
                return Err(format!(
                    "two of a profile's controls share an address or a place: {:?} and {:?}",
                    other.address, place.address
                ));
            }
        }
        let modes = [self.modes.steady, self.modes.pulse, self.modes.blink];
        if modes
            .iter()
            .flatten()
            .any(|mode| mode.channel.is_some_and(|c| c > 15) || mode.offset > 127)
        {
            return Err("a mode's channel is 0–15 and its offset 0–127".into());
        }
        if self.dim.is_some_and(|dim| !(0.0..=1.0).contains(&dim)) {
            return Err("a profile's dim is 0 to 1".into());
        }
        match &self.color {
            Some(ColorOut::Palette(palette))
                if palette.is_empty() || palette.keys().any(|v| *v > 127) =>
            {
                Err("a palette maps velocities 0–127 to colors, and has at least one".into())
            }
            Some(ColorOut::Sysex(template)) => sysex(template, 0, [0; 3]).map(|_| ()),
            _ => Ok(()),
        }
    }

    /// The message that shows `look` on the control at `address`, `phase`
    /// being whether a blink drawn here is lit now; `None` where nothing
    /// can show. A fader on a CC is sent its position, for motor faders and
    /// rings.
    pub fn draw(&self, address: &Address, look: &Look, phase: bool) -> Option<Vec<u8>> {
        match address {
            Address::MidiCc { channel, number } if look.job == Job::Fader => Some(vec![
                0xb0 | channel,
                *number,
                (look.value.clamp(0., 1.) * 127.).round() as u8,
            ]),
            _ => self.show(address, look.light(), phase),
        }
    }

    /// The message that shows `light` on the control at `address`.
    pub fn show(&self, address: &Address, light: Light, phase: bool) -> Option<Vec<u8>> {
        let (status, channel, number) = match *address {
            Address::MidiNote { channel, number } => (0x90, channel, number),
            Address::MidiCc { channel, number } => (0xb0, channel, number),
            Address::Osc { .. } => return None,
        };
        let mode = match (&self.color, light.shine) {
            (Some(ColorOut::Sysex(_)), _) => None,
            (_, Shine::Steady) => self.modes.steady,
            (_, Shine::Pulse) => self.modes.pulse,
            (_, Shine::Blink) => self.modes.blink,
        };
        // A blink the device cannot show is drawn here, dark half the time;
        // a pulse it cannot show is steady.
        let dark = light.shine == Shine::Blink && mode.is_none() && !phase;
        let rgb = if dark {
            [0.; 3]
        } else if light.bright {
            light.color
        } else {
            light.color.map(|c| c * self.dim.unwrap_or(DIM))
        };
        let sent = mode.or(self.modes.steady).unwrap_or_default();
        let (channel, offset) = (sent.channel.unwrap_or(channel), sent.offset);
        let velocity = match &self.color {
            None => {
                if light.bright && !dark {
                    127
                } else {
                    0
                }
            }
            Some(ColorOut::Palette(palette)) => nearest(palette, device(rgb, 255.)),
            Some(ColorOut::Sysex(template)) => {
                return sysex(template, number, device(rgb, 127.)).ok();
            }
        };
        Some(vec![
            status | channel,
            number,
            velocity.saturating_add(offset).min(127),
        ])
    }

    /// What putting `page` on this controller binds: each control of the
    /// profile at a place some cell covers takes that cell's next part of
    /// its kind, a fader a fader and a pad one pressed, in the cell's
    /// places row by row. A list one place big is its Next.
    pub fn map(
        &self,
        page: &LayoutPage,
        patterns: &[PerformPattern],
    ) -> Vec<(String, Address, Mode)> {
        let mut bound = Vec::new();
        for cell in &page.cells {
            let mut parts: Vec<(String, Mode)> = names::parts(cell, patterns)
                .into_iter()
                .filter(|part| {
                    let single = (cell.width, cell.height) == (1, 1);
                    !(single && matches!(cell.content, CellContent::List { .. }))
                        || part.kind == PartKind::Next
                })
                .filter_map(|part| Some((part.control, part.kind.mode()?)))
                .collect();
            for row in cell.row..cell.row + cell.height {
                for column in cell.column..cell.column + cell.width {
                    let Some(place) = self
                        .controls
                        .iter()
                        .find(|place| (place.column, place.row) == (column, row))
                    else {
                        continue;
                    };
                    let wanted = if place.fader {
                        Mode::Fader
                    } else {
                        Mode::Momentary
                    };
                    if let Some(at) = parts.iter().position(|(_, mode)| *mode == wanted) {
                        let (control, mode) = parts.remove(at);
                        bound.push((control, place.address.clone(), mode));
                    }
                }
            }
        }
        bound
    }
}

/// A light color (linear Rec. 2020) as a device's channels 0–`top`, as a
/// screen shows it.
fn device(rgb: [f64; 3], top: f64) -> [u8; 3] {
    color_space::to_display_srgb(rgb).map(|c| (c.clamp(0., 1.) * top).round() as u8)
}

/// The palette's velocity whose color is nearest `rgb`.
fn nearest(palette: &BTreeMap<u8, [u8; 3]>, rgb: [u8; 3]) -> u8 {
    let distance = |shown: &[u8; 3]| -> i32 {
        (0..3)
            .map(|c| (i32::from(shown[c]) - i32::from(rgb[c])).pow(2))
            .sum()
    };
    palette
        .iter()
        .min_by_key(|(_, shown)| distance(shown))
        .map_or(0, |(velocity, _)| *velocity)
}

/// `template` filled for control `number` in `rgb` (each 0–127).
fn sysex(template: &str, number: u8, rgb: [u8; 3]) -> Result<Vec<u8>, String> {
    let bytes = template
        .split_whitespace()
        .map(|token| match token {
            "{number}" => Ok(number),
            "{r}" => Ok(rgb[0]),
            "{g}" => Ok(rgb[1]),
            "{b}" => Ok(rgb[2]),
            hex => u8::from_str_radix(hex, 16).map_err(|_| {
                format!("a SysEx template is hex bytes and {{number}}, {{r}}, {{g}}, {{b}}; got {hex:?}")
            }),
        })
        .collect::<Result<Vec<u8>, String>>()?;
    match (bytes.first(), bytes.last()) {
        (Some(0xf0), Some(0xf7)) if bytes.len() > 2 => Ok(bytes),
        _ => Err("a SysEx template starts with F0 and ends with F7".into()),
    }
}

#[cfg(test)]
mod tests;
