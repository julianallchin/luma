use luma_scene::environment::{VenueEnvironment, VenueHaze};
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
    /// [`ROLE_OWNER`] when this is the reader's own venue, [`ROLE_MEMBER`]
    /// when it was shared with them. Who they are to it, for display; what they
    /// may do is [`Self::capability`]. Both are the reader's, not the row's:
    /// set by [`crate::database::local::venues`] from the admission.
    #[sqlx(skip)]
    pub role: String,
    #[sqlx(skip)]
    pub capability: VenueCapability,
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

/// What the reader may do with a venue, as the venue's admission decides it:
/// the `auth_venue_access` view, read as
/// [`crate::database::local::venue_access::Standing`]. Callers read this; none
/// of them re-derives it by comparing `uid`s.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VenueCapability {
    /// Change anything in it — its rig, stage, groups, perform set and every
    /// score, whoever wrote it.
    pub edit: bool,
    /// Change who has it: its share code and its memberships.
    pub manage: bool,
}

impl Venue {
    pub fn is_owner(&self) -> bool {
        self.role == ROLE_OWNER
    }

    pub fn is_member(&self) -> bool {
        self.role == ROLE_MEMBER
    }
}

/// Someone who can open a venue, as its owner sees them.
///
/// Only an id: the server keeps no profile another account may read, so the
/// owner sees who joined and when, not their address.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VenueMember {
    pub uid: String,
    /// [`ROLE_OWNER`] for the venue's owner, [`ROLE_MEMBER`] for everyone who
    /// joined with a code.
    pub role: String,
    /// When they got access: the venue's creation for its owner, the
    /// membership row's for everyone else.
    pub since: String,
    /// Whether this is the signed-in account.
    pub is_you: bool,
}

/// The letters a share code is written in: digits and capitals with every
/// pair a person could misread or mistype left out (`0`/`O`, `1`/`I`/`L`) and
/// `U`, as Crockford's base 32 does, so fewer codes spell words. Thirty
/// symbols, eight places: 6.6 × 10¹¹ codes.
pub const SHARE_CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const SHARE_CODE_LEN: usize = 8;

/// The stored form of whatever a person typed or pasted, or `None` when it
/// cannot be a code.
///
/// Case, spaces and the display hyphen are forgiven; a letter the alphabet
/// leaves out is not, because no code contains one.
pub fn normalize_share_code(input: &str) -> Option<String> {
    let code: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let valid = code.len() == SHARE_CODE_LEN
        && code.bytes().all(|byte| SHARE_CODE_ALPHABET.contains(&byte));
    valid.then_some(code)
}

/// A code as people read it: two groups of four, `K7M4-Q9TX`.
pub fn display_share_code(code: &str) -> String {
    if code.len() == SHARE_CODE_LEN && code.is_ascii() {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_code_is_forgiven_its_case_spacing_and_hyphen_but_not_its_letters() {
        assert_eq!(
            normalize_share_code(" k7m4-q9tx ").as_deref(),
            Some("K7M4Q9TX")
        );
        assert_eq!(
            normalize_share_code("K7M4 Q9TX").as_deref(),
            Some("K7M4Q9TX")
        );
        // `O`, `0`, `I`, `1` and `L` are never in a code.
        for wrong in ["K7M4Q9TO", "K7M4Q9T0", "K7M4Q9TI", "K7M4Q9T1", "K7M4Q9TL"] {
            assert_eq!(normalize_share_code(wrong), None, "{wrong}");
        }
        assert_eq!(normalize_share_code("K7M4Q9T"), None);
        assert_eq!(normalize_share_code(""), None);
        assert_eq!(display_share_code("K7M4Q9TX"), "K7M4-Q9TX");
    }
}
