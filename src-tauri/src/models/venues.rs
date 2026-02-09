use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venues.ts")]
#[ts(rename_all = "camelCase")]
pub struct Venue {
    #[ts(type = "number")]
    pub id: i64,
    #[sqlx(rename = "remote_id")]
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    pub name: String,
    pub description: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

/// Per-venue override of which implementation to use for a pattern
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venues.ts")]
#[ts(rename_all = "camelCase")]
pub struct VenueImplementationOverride {
    #[ts(type = "number")]
    #[sqlx(rename = "venue_id")]
    pub venue_id: i64,
    #[ts(type = "number")]
    #[sqlx(rename = "pattern_id")]
    pub pattern_id: i64,
    #[ts(type = "number")]
    #[sqlx(rename = "implementation_id")]
    pub implementation_id: i64,
    #[sqlx(rename = "remote_id")]
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}
