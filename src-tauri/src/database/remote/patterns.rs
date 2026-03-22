// Remote CRUD operations for patterns table

use super::common::{SupabaseClient, SyncError};
use crate::models::patterns::PatternSummary;
use serde::{Deserialize, Serialize};

/// Payload for upserting a pattern to Supabase
#[derive(Serialize)]
struct PatternPayload<'a> {
    id: &'a str,
    uid: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category_id: Option<&'a str>,
    is_published: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    author_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    forked_from_id: Option<&'a str>,
}

/// Upsert a pattern in Supabase (idempotent).
///
/// The local UUID is sent as the cloud `id`. Uses ON CONFLICT(id) upsert.
/// The category_id is taken directly from the pattern (already a UUID).
pub async fn upsert_pattern(
    client: &SupabaseClient,
    pattern: &PatternSummary,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = pattern
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = PatternPayload {
        id: &pattern.id,
        uid,
        name: &pattern.name,
        description: pattern.description.as_deref(),
        category_id: pattern.category_id.as_deref(),
        is_published: pattern.is_published,
        author_name: pattern.author_name.as_deref(),
        forked_from_id: pattern.forked_from_id.as_deref(),
    };

    client
        .upsert_no_return("patterns", &payload, "id", access_token)
        .await
}

/// Delete a pattern from Supabase
pub async fn delete_pattern(
    client: &SupabaseClient,
    id: &str,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("patterns", id, access_token).await
}

/// Row returned when fetching patterns from Supabase
#[derive(Deserialize)]
pub struct RemotePatternRow {
    pub id: String,
    pub uid: String,
    pub name: String,
    pub description: Option<String>,
    pub is_published: bool,
    pub author_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Fetch all published patterns from Supabase
pub async fn fetch_published_patterns(
    client: &SupabaseClient,
    access_token: &str,
) -> Result<Vec<RemotePatternRow>, SyncError> {
    client
        .select(
            "patterns",
            "is_published=eq.true&select=id,uid,name,description,is_published,author_name,created_at,updated_at",
            access_token,
        )
        .await
}

/// Fetch all patterns belonging to a specific user from Supabase (regardless of published status)
pub async fn fetch_own_patterns(
    client: &SupabaseClient,
    uid: &str,
    access_token: &str,
) -> Result<Vec<RemotePatternRow>, SyncError> {
    client
        .select(
            "patterns",
            &format!("uid=eq.{}&select=id,uid,name,description,is_published,author_name,created_at,updated_at", uid),
            access_token,
        )
        .await
}

/// Row returned when fetching a user's profile from Supabase
#[derive(Deserialize)]
pub struct ProfileRow {
    pub display_name: Option<String>,
}

/// Fetch the current user's display_name from the profiles table
pub async fn fetch_user_profile(
    client: &SupabaseClient,
    uid: &str,
    access_token: &str,
) -> Result<Option<String>, SyncError> {
    let rows: Vec<ProfileRow> = client
        .select(
            "profiles",
            &format!("id=eq.{}&select=display_name", uid),
            access_token,
        )
        .await?;
    Ok(rows.into_iter().next().and_then(|r| r.display_name))
}
