use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
    #[serde(rename = "$value")]
    pub label: String,
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
