use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum ChannelType {
    Intensity,
    Colour,
    Pan,
    Tilt,
    Beam,
    Shutter,
    Speed,
    Gobo,
    Prism,
    Effect,
    Maintenance,
    Nothing,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum ChannelColour {
    Red,
    Green,
    Blue,
    White,
    Amber,
    UV,
    Cyan,
    Magenta,
    Yellow,
    Lime,
    None,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct FixtureDefinition {
    pub manufacturer: String,
    pub model: String,
    #[serde(rename = "Type")]
    pub type_: String,
    #[serde(rename = "Channel", default)]
    pub channels: Vec<Channel>,
    #[serde(rename = "Mode", default)]
    pub modes: Vec<Mode>,
    pub physical: Option<Physical>,
    /// Set on a definition lowered from the fixture library (`lib:<id>`):
    /// the spec's build and travel, which the QLC+ fields cannot carry.
    #[serde(skip)]
    pub library: Option<std::sync::Arc<crate::fixtures::library::Source>>,
}

/// The QXF `<Type>` of an ILDA laser projector: a fixture Luma draws beams
/// with from laser clips, through a point stream rather than DMX.
pub const LASER_PROJECTOR: &str = "Laser Projector";

/// The bundled definition of the ILDA laser projector: the one fixture Luma
/// offers that the fixture library does not carry.
pub const LASER_PROJECTOR_PATH: &str = "Generic/Generic-ILDA-Laser-Projector.qxf";

impl FixtureDefinition {
    /// Whether Luma can drive `mode`'s light: false for a library mode it
    /// cannot light (its colour only through presets of unknown colour).
    /// QLC+ definitions are taken as they come.
    pub fn supports_mode(&self, mode: &str) -> bool {
        self.library
            .as_ref()
            .and_then(|source| source.light.get(mode))
            .is_none_or(|light| light.lights())
    }

    /// Whether this is an ILDA laser projector ([`LASER_PROJECTOR`]).
    pub fn is_laser_projector(&self) -> bool {
        self.type_ == LASER_PROJECTOR
    }

    /// Pan and tilt travel, `[lo, hi]` degrees from home. A library fixture's
    /// is its spec's joint range, which can be asymmetric. A QLC+ profile's
    /// focus block gives only the full sweep (540 and 270 when it says
    /// nothing), centred on home.
    pub fn travel(&self) -> [[f32; 2]; 2] {
        if let Some(travel) = self.library.as_ref().map(|l| l.travel) {
            return travel;
        }
        let focus = self.physical.as_ref().and_then(|p| p.focus.as_ref());
        [
            focus.and_then(|f| f.pan_max).unwrap_or(540),
            focus.and_then(|f| f.tilt_max).unwrap_or(270),
        ]
        .map(|full| [-(full as f32) / 2.0, full as f32 / 2.0])
    }

    /// Whether `mode` has a pan channel and a tilt channel.
    pub fn moves(&self, mode: &Mode) -> [bool; 2] {
        let has = |kind: ChannelType| {
            mode.channels.iter().any(|mode_channel| {
                self.channels
                    .iter()
                    .find(|c| c.name == mode_channel.name)
                    .is_some_and(|c| c.get_type() == kind)
            })
        };
        [has(ChannelType::Pan), has(ChannelType::Tilt)]
    }

    /// Returns true if the given mode has a color wheel (Colour or Gobo channel with color capabilities).
    /// Color wheel fixtures need the color's luminance applied to the dimmer since
    /// the wheel can only select discrete colors, not brightness.
    pub fn has_color_wheel(&self, mode: &Mode) -> bool {
        mode.channels.iter().any(|mode_channel| {
            let channel = match self.channels.iter().find(|c| c.name == mode_channel.name) {
                Some(c) => c,
                None => return false,
            };
            let ch_type = channel.get_type();
            let ch_colour = channel.get_colour();
            // Color wheel channels are Colour or Gobo type with no specific color (None)
            // and have capabilities defining the color slots
            (ch_type == ChannelType::Colour || ch_type == ChannelType::Gobo)
                && ch_colour == ChannelColour::None
                && !channel.capabilities.is_empty()
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Channel {
    #[serde(rename = "@Name")]
    pub name: String,
    #[serde(rename = "@Preset", default)]
    pub preset: Option<String>,
    pub group: Option<Group>,
    #[serde(rename = "Capability", default)]
    pub capabilities: Vec<Capability>,
    /// Set on a channel lowered from the fixture library: the byte of a
    /// resolved value it is, which says what it is without any name sniffing.
    #[serde(skip)]
    pub lowered: Option<Box<crate::fixtures::library::Lowered>>,
}

impl Channel {
    pub fn get_type(&self) -> ChannelType {
        if let Some(lowered) = &self.lowered {
            return lowered.channel_type();
        }
        if let Some(preset) = &self.preset {
            if preset.starts_with("Intensity") {
                return ChannelType::Intensity;
            }
            if preset.starts_with("Color") || preset.starts_with("Colour") {
                return ChannelType::Colour;
            } // Generic color
            if preset.starts_with("Speed") {
                // `SpeedPanTiltFastSlow` and its kin name pan and tilt too,
                // but they are the motor speed, not a position.
                return ChannelType::Speed;
            }
            if preset.contains("Pan") {
                return ChannelType::Pan;
            }
            if preset.contains("Tilt") {
                return ChannelType::Tilt;
            }
            if preset.contains("Shutter") || preset.contains("Strobe") {
                return ChannelType::Shutter;
            }
        }

        if let Some(group) = &self.group {
            match group.value.as_str() {
                "Intensity" => ChannelType::Intensity,
                "Colour" => ChannelType::Colour,
                "Pan" => ChannelType::Pan,
                "Tilt" => ChannelType::Tilt,
                "Beam" => ChannelType::Beam,
                "Shutter" => ChannelType::Shutter,
                "Speed" => ChannelType::Speed,
                "Gobo" => ChannelType::Gobo,
                "Prism" => ChannelType::Prism,
                "Effect" => ChannelType::Effect,
                "Maintenance" => ChannelType::Maintenance,
                "Nothing" => ChannelType::Nothing,
                _ => ChannelType::Unknown,
            }
        } else {
            // Fallback to name parsing
            let name = self.name.to_lowercase();
            if name.contains("red")
                || name.contains("green")
                || name.contains("blue")
                || name.contains("white")
                || name.contains("amber")
                || name.contains("color")
            {
                ChannelType::Intensity // QLC+ treats RGB as Intensity group often, or Colour group?
                                       // Actually QLC+ separates 'Intensity' (Dimmer) from 'Colour' (RGB).
                                       // But often RGB channels are in 'Intensity' group in QLC+ files (check XML above: Group Byte=0 is Intensity).
                                       // Wait, in the XML above, Red/Green/Blue do NOT have a Group tag!
                                       // They only have Preset="IntensityRed".
                                       // So Preset is the primary source.
            } else if name.contains("dimmer") {
                ChannelType::Intensity
            } else if name.contains("strobe") || name.contains("shutter") {
                ChannelType::Shutter
            } else if name.contains("pan") {
                ChannelType::Pan
            } else if name.contains("tilt") {
                ChannelType::Tilt
            } else {
                ChannelType::Unknown
            }
        }
    }

    /// Whether this channel changes the light's *colour* at all.
    ///
    /// A different question from [`Channel::get_colour`], which answers "which
    /// of the primaries this codebase mixes does it drive" and so has to return
    /// one of a closed set the DMX path can act on. QLC+ files carry colour
    /// controls that answer the first question and not the second — an HSV par
    /// is `IntensityHue` / `IntensitySaturation`, a six-colour engine adds
    /// `IntensityIndigo` — and a classifier that only asked
    /// the mixer's question called every full-colour par patched in HSV mode a
    /// colourless dimmer.
    #[must_use]
    pub fn carries_colour(&self) -> bool {
        if self.get_colour() != ChannelColour::None {
            return true;
        }
        self.preset.as_deref().is_some_and(|preset| {
            matches!(
                preset,
                "IntensityHue" | "IntensitySaturation" | "IntensityIndigo"
            )
        })
    }

    pub fn get_colour(&self) -> ChannelColour {
        if let Some(lowered) = &self.lowered {
            return lowered.channel_colour();
        }
        if let Some(preset) = &self.preset {
            match preset.as_str() {
                "IntensityRed" => return ChannelColour::Red,
                "IntensityGreen" => return ChannelColour::Green,
                "IntensityBlue" => return ChannelColour::Blue,
                "IntensityWhite" => return ChannelColour::White,
                "IntensityAmber" => return ChannelColour::Amber,
                "IntensityUV" => return ChannelColour::UV,
                "IntensityCyan" => return ChannelColour::Cyan,
                "IntensityMagenta" => return ChannelColour::Magenta,
                "IntensityYellow" => return ChannelColour::Yellow,
                "IntensityLime" => return ChannelColour::Lime,
                _ => {}
            }
        }

        // Fallback to name
        let name = self.name.to_lowercase();
        if name.contains("red") {
            ChannelColour::Red
        } else if name.contains("green") {
            ChannelColour::Green
        } else if name.contains("blue") {
            ChannelColour::Blue
        } else if name.contains("white") {
            ChannelColour::White
        } else if name.contains("amber") {
            ChannelColour::Amber
        } else if name.contains("uv") {
            ChannelColour::UV
        } else if name.contains("cyan") {
            ChannelColour::Cyan
        } else if name.contains("magenta") {
            ChannelColour::Magenta
        } else if name.contains("yellow") {
            ChannelColour::Yellow
        } else if name.contains("lime") {
            ChannelColour::Lime
        } else {
            ChannelColour::None
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Group {
    #[serde(rename = "@Byte")]
    pub byte: u8,
    #[serde(rename = "$value")]
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Capability {
    #[serde(rename = "@Min")]
    pub min: u8,
    #[serde(rename = "@Max")]
    pub max: u8,
    #[serde(rename = "@Preset", default)]
    pub preset: Option<String>,
    // QLC+ fixture definitions use Res1/Res2 for capability resources.
    // Keep legacy fields too for older definitions.
    #[serde(rename = "@Res1", default)]
    pub res1: Option<String>,
    #[serde(rename = "@Res2", default)]
    pub res2: Option<String>,
    #[serde(rename = "@Res", default)]
    pub res: Option<String>,
    #[serde(rename = "@Color", default)]
    pub color: Option<String>,
    #[serde(rename = "@Color2", default)]
    pub color_2: Option<String>,
    #[serde(rename = "$value")]
    pub label: String,
}

impl Capability {
    pub fn is_strobe(&self) -> bool {
        if let Some(preset) = &self.preset {
            if preset.contains("Strobe") {
                return true;
            }
        }
        let label = self.label.to_lowercase();
        label.contains("strobe")
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Mode {
    #[serde(rename = "@Name")]
    pub name: String,
    #[serde(rename = "Channel", default)]
    pub channels: Vec<ModeChannel>,
    #[serde(rename = "Head", default)]
    pub heads: Vec<Head>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModeChannel {
    #[serde(rename = "@Number")]
    pub number: u32,
    #[serde(rename = "$value")]
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Head {
    #[serde(rename = "Channel")]
    pub channels: Vec<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Physical {
    pub dimensions: Option<Dimensions>,
    pub layout: Option<Layout>,
    pub bulb: Option<Bulb>,
    pub lens: Option<Lens>,
    pub focus: Option<Focus>,
    pub technical: Option<Technical>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Bulb {
    #[serde(rename = "@Type")]
    pub type_: Option<String>,
    #[serde(rename = "@Lumens")]
    pub lumens: Option<u32>,
    /// Lumens out of the whole fixture, from a datasheet. Not QLC+'s: a
    /// library fixture lowers its `optics.lumens` here.
    #[serde(
        rename = "@OutputLumens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_lumens: Option<f32>,
    #[serde(rename = "@ColourTemperature")]
    pub colour_temperature: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Lens {
    #[serde(rename = "@Name")]
    pub name: Option<String>,
    #[serde(rename = "@DegreesMin")]
    pub degrees_min: Option<f32>,
    #[serde(rename = "@DegreesMax")]
    pub degrees_max: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Focus {
    #[serde(rename = "@Type")]
    pub type_: Option<String>,
    #[serde(rename = "@PanMax")]
    pub pan_max: Option<u32>,
    #[serde(rename = "@TiltMax")]
    pub tilt_max: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Technical {
    #[serde(rename = "@PowerConsumption")]
    pub power_consumption: Option<u32>,
    #[serde(rename = "@DmxConnector")]
    pub dmx_connector: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Dimensions {
    #[serde(rename = "@Weight")]
    pub weight: f32,
    #[serde(rename = "@Width")]
    pub width: f32,
    #[serde(rename = "@Height")]
    pub height: f32,
    #[serde(rename = "@Depth")]
    pub depth: f32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Layout {
    #[serde(rename = "@Width")]
    pub width: u32,
    #[serde(rename = "@Height")]
    pub height: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FixtureEntry {
    pub manufacturer: String,
    pub model: String,
    /// What a patch row stores in `fixture_path`: a QLC+ file relative to the
    /// fixtures root (`Acme/Dotline180.qxf`), or `lib:<id>`.
    pub path: String,
    /// A library fixture's index entry: its modes with their footprints and
    /// how well it has been checked. `None` for a QLC+ file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<crate::fixtures::library::Entry>,
}

/// A fixture that has been patched to a venue
#[derive(Debug, Serialize, Deserialize, Clone, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PatchedFixture {
    pub id: String,
    pub uid: Option<String>,
    pub venue_id: String,
    pub universe: i64,
    pub address: i64,
    pub num_channels: i64,
    pub manufacturer: String,
    pub model: String,
    pub mode_name: String,
    pub fixture_path: String,
    pub label: Option<String>,
    /// The address was set by hand, so auto-patch derives around it rather than
    /// over it. Local-only.
    pub address_pinned: bool,
}

/// The QLC+ subset the renderer reads, out of the QLC+ model Luma parsed.
///
/// This module's types and [`luma_scene::definition`] are the same concept — a
/// `.qxf` file — declared twice, and this function is the standing cost of
/// that. It should collapse to one set of types that the golden JSON also
/// deserialises into.
impl From<&FixtureDefinition> for luma_scene::definition::Definition {
    fn from(def: &FixtureDefinition) -> Self {
        Self {
            built: def.library.as_ref().map(|l| l.built.clone()),
            kind: def.type_.clone(),
            modes: def
                .modes
                .iter()
                .map(|mode| luma_scene::definition::Mode {
                    name: mode.name.clone(),
                    // Only the length is read (`Definition::head_count`); the
                    // renderer's head is deliberately opaque.
                    heads: mode.heads.iter().map(|_| serde_json::Value::Null).collect(),
                })
                .collect(),
            physical: def
                .physical
                .as_ref()
                .map(|p| luma_scene::definition::Physical {
                    dimensions: p
                        .dimensions
                        .as_ref()
                        .map(|d| luma_scene::definition::Dimensions {
                            width: d.width,
                            height: d.height,
                            depth: d.depth,
                        }),
                    layout: p.layout.as_ref().map(|l| luma_scene::definition::Layout {
                        width: l.width,
                        height: l.height,
                    }),
                    // Zero is the renderer's "unknown", which is what an absent QLC+
                    // attribute means.
                    lens: p.lens.as_ref().map(|l| luma_scene::definition::Lens {
                        degrees_min: l.degrees_min.unwrap_or(0.0),
                        degrees_max: l.degrees_max.unwrap_or(0.0),
                        // QLC+ has no lens size or position; spec-sheet values
                        // go here when the library carries them.
                        radius_m: None,
                        offset_m: None,
                    }),
                    bulb: p.bulb.as_ref().map(|b| luma_scene::definition::Bulb {
                        lumens: b.lumens.map(|l| l as f32),
                        output_lumens: b.output_lumens,
                    }),
                }),
        }
    }
}
