use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Serialize, Deserialize, Clone, TS, PartialEq)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
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

#[derive(Debug, Serialize, Deserialize, Clone, TS, PartialEq)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
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
    None,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
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
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "PascalCase")]
pub struct Channel {
    #[serde(rename = "@Name")]
    pub name: String,
    #[serde(rename = "@Preset", default)]
    pub preset: Option<String>,
    pub group: Option<Group>,
    #[serde(rename = "Capability", default)]
    pub capabilities: Vec<Capability>,
}

impl Channel {
    pub fn get_type(&self) -> ChannelType {
        if let Some(preset) = &self.preset {
            if preset.starts_with("Intensity") {
                return ChannelType::Intensity;
            }
            if preset.starts_with("Color") || preset.starts_with("Colour") {
                return ChannelType::Colour;
            } // Generic color
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

    pub fn get_colour(&self) -> ChannelColour {
        if let Some(preset) = &self.preset {
            match preset.as_str() {
                "IntensityRed" => return ChannelColour::Red,
                "IntensityGreen" => return ChannelColour::Green,
                "IntensityBlue" => return ChannelColour::Blue,
                "IntensityWhite" => return ChannelColour::White,
                "IntensityAmber" => return ChannelColour::Amber,
                "IntensityUV" => return ChannelColour::UV,
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
        } else {
            ChannelColour::None
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "PascalCase")]
pub struct Group {
    #[serde(rename = "@Byte")]
    pub byte: u8,
    #[serde(rename = "$value")]
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "PascalCase")]
pub struct Capability {
    #[serde(rename = "@Min")]
    pub min: u8,
    #[serde(rename = "@Max")]
    pub max: u8,
    #[serde(rename = "@Preset", default)]
    pub preset: Option<String>,
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

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "PascalCase")]
pub struct Mode {
    #[serde(rename = "@Name")]
    pub name: String,
    #[serde(rename = "Channel", default)]
    pub channels: Vec<ModeChannel>,
    #[serde(rename = "Head", default)]
    pub heads: Vec<Head>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
pub struct ModeChannel {
    #[serde(rename = "@Number")]
    pub number: u32,
    #[serde(rename = "$value")]
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
pub struct Head {
    #[serde(rename = "Channel")]
    pub channels: Vec<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "PascalCase")]
pub struct Physical {
    pub dimensions: Option<Dimensions>,
    pub layout: Option<Layout>,
    pub bulb: Option<Bulb>,
    pub lens: Option<Lens>,
    pub focus: Option<Focus>,
    pub technical: Option<Technical>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "PascalCase")]
pub struct Bulb {
    #[serde(rename = "@Type")]
    pub type_: Option<String>,
    #[serde(rename = "@Lumens")]
    pub lumens: Option<u32>,
    #[serde(rename = "@ColourTemperature")]
    pub colour_temperature: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "PascalCase")]
pub struct Lens {
    #[serde(rename = "@Name")]
    pub name: Option<String>,
    #[serde(rename = "@DegreesMin")]
    pub degrees_min: Option<f32>,
    #[serde(rename = "@DegreesMax")]
    pub degrees_max: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "PascalCase")]
pub struct Focus {
    #[serde(rename = "@Type")]
    pub type_: Option<String>,
    #[serde(rename = "@PanMax")]
    pub pan_max: Option<u32>,
    #[serde(rename = "@TiltMax")]
    pub tilt_max: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "PascalCase")]
pub struct Technical {
    #[serde(rename = "@PowerConsumption")]
    pub power_consumption: Option<u32>,
    #[serde(rename = "@DmxConnector")]
    pub dmx_connector: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
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

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "PascalCase")]
pub struct Layout {
    #[serde(rename = "@Width")]
    pub width: u32,
    #[serde(rename = "@Height")]
    pub height: u32,
}

#[derive(Debug, Serialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
pub struct FixtureEntry {
    pub manufacturer: String,
    pub model: String,
    pub path: String, // Relative to fixtures root, e.g., "Acme/Dotline180.qxf"
}

#[derive(Debug, Serialize, Deserialize, Clone, TS, sqlx::FromRow)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "camelCase")]
pub struct PatchedFixture {
    pub id: String,
    pub universe: i64,
    pub address: i64,
    pub num_channels: i64, // Renamed and changed type to i64 for SQL
    pub manufacturer: String,
    pub model: String,
    pub mode_name: String,
    pub fixture_path: String,
    pub label: Option<String>,
    pub pos_x: f64, // Added spatial data
    pub pos_y: f64,
    pub pos_z: f64,
    pub rot_x: f64,
    pub rot_y: f64,
    pub rot_z: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "camelCase")]
pub enum FixtureNodeType {
    Fixture,
    Head,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/fixtures.ts")]
#[serde(rename_all = "camelCase")]
pub struct FixtureNode {
    pub id: String,
    pub label: String,
    pub type_: FixtureNodeType,
    pub children: Vec<FixtureNode>,
}
