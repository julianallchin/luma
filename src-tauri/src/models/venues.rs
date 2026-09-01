use luma_render::scene_desc::VenueEnvironment;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

/// Venue role constants
pub const ROLE_OWNER: &str = "owner";
pub const ROLE_MEMBER: &str = "member";

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venues.ts")]
#[ts(rename_all = "camelCase")]
pub struct Venue {
    pub id: String,
    pub uid: Option<String>,
    pub name: String,
    pub description: Option<String>,
    #[sqlx(rename = "share_code")]
    pub share_code: Option<String>,
    pub role: String,
    #[sqlx(rename = "controller_port")]
    pub controller_port: Option<String>,
    #[sqlx(rename = "mixer_port")]
    pub mixer_port: Option<String>,
    #[sqlx(rename = "mixer_mapping_json")]
    pub mixer_mapping_json: Option<String>,
    /// What kind of room this is and how far up its one dial is.
    ///
    /// Venue truth, at the tier of the name: every picture of this venue — the
    /// editor viewport, an agent's offscreen frame — is taken under it, and
    /// `luma_render::house` is the only place it becomes light.
    ///
    /// Stored as the type's own JSON (`sqlx(try_from)` decodes it, and the
    /// decode is total — see that type's `From<String>`), so the column, the
    /// wire and `luma.venue.environment()` are one string. Local-only: it is
    /// absent from `sync::registry`'s `venues` columns.
    #[sqlx(try_from = "String")]
    #[ts(
        type = "{ mode: \"indoor\", houseLevel: number } | { mode: \"outdoor\", sunElevationDeg: number }"
    )]
    pub environment: VenueEnvironment,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

impl Venue {
    pub fn is_owner(&self) -> bool {
        self.role == ROLE_OWNER
    }

    pub fn is_member(&self) -> bool {
        self.role == ROLE_MEMBER
    }
}

/// Per-venue override of which implementation to use for a pattern
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/venues.ts")]
#[ts(rename_all = "camelCase")]
pub struct VenueImplementationOverride {
    #[sqlx(rename = "venue_id")]
    pub venue_id: String,
    #[sqlx(rename = "pattern_id")]
    pub pattern_id: String,
    #[sqlx(rename = "implementation_id")]
    pub implementation_id: String,
    pub uid: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}
