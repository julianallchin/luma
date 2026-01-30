use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

/// Tag category
#[derive(Debug, Serialize, Deserialize, Clone, TS, PartialEq, Eq)]
#[ts(export, export_to = "../../src/bindings/tags.ts")]
#[serde(rename_all = "snake_case")]
pub enum TagCategory {
    Spatial,
    Purpose,
    Meta,
}

impl From<&str> for TagCategory {
    fn from(s: &str) -> Self {
        match s {
            "spatial" => TagCategory::Spatial,
            "purpose" => TagCategory::Purpose,
            "meta" => TagCategory::Meta,
            _ => TagCategory::Purpose,
        }
    }
}

impl std::fmt::Display for TagCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TagCategory::Spatial => write!(f, "spatial"),
            TagCategory::Purpose => write!(f, "purpose"),
            TagCategory::Meta => write!(f, "meta"),
        }
    }
}

/// A tag that can be assigned to fixtures
#[derive(Debug, Serialize, Deserialize, Clone, TS, FromRow)]
#[ts(export, export_to = "../../src/bindings/tags.ts")]
#[serde(rename_all = "camelCase")]
pub struct FixtureTag {
    #[ts(type = "number")]
    pub id: i64,
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    #[ts(type = "number")]
    pub venue_id: i64,
    pub name: String,
    pub category: String,
    pub is_auto_generated: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Tag with assignment count for UI display
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/tags.ts")]
#[serde(rename_all = "camelCase")]
pub struct TagWithCount {
    pub tag: FixtureTag,
    pub fixture_count: usize,
}

/// Density mode for selection
#[derive(Debug, Serialize, Deserialize, Clone, TS, Default, PartialEq, Eq)]
#[ts(export, export_to = "../../src/bindings/tags.ts")]
#[serde(rename_all = "snake_case")]
pub enum SelectionDensity {
    #[default]
    All,
    OneGroup,
}

/// Whether spatial attributes are computed relative to group or global
#[derive(Debug, Serialize, Deserialize, Clone, TS, Default, PartialEq, Eq)]
#[ts(export, export_to = "../../src/bindings/tags.ts")]
#[serde(rename_all = "snake_case")]
pub enum SpatialReference {
    #[default]
    Global,
    GroupLocal,
}

/// Tag-based selection configuration
#[derive(Debug, Serialize, Deserialize, Clone, TS, Default)]
#[ts(export, export_to = "../../src/bindings/tags.ts")]
#[serde(rename_all = "camelCase")]
pub struct TagSelectionConfig {
    /// Tag expression (e.g., "left & blinder")
    pub expression: String,
    /// Density: all fixtures or one selection per group
    pub density: SelectionDensity,
    /// Spatial reference: global or group-relative
    pub spatial_reference: SpatialReference,
}
