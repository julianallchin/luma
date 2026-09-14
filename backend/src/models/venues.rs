use luma_render::scene_desc::{VenueEnvironment, VenueHaze};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Venue role constants
pub const ROLE_OWNER: &str = "owner";
pub const ROLE_MEMBER: &str = "member";

#[derive(Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
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
    /// wire and `luma.venue.environment()` are one string, synced with the venue.
    #[sqlx(try_from = "String")]
    pub environment: VenueEnvironment,
    /// How hazy this room is, and what its haze looks like.
    ///
    /// Venue truth beside [`Self::environment`], and stored the same way: the
    /// type's own JSON, decoded totally, synced with the venue. The march's
    /// cost knobs (`steps`, `resolution`) are deliberately not here — they are
    /// local to whichever machine draws the frame.
    #[sqlx(try_from = "String")]
    pub haze: VenueHaze,
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
#[derive(Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
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
