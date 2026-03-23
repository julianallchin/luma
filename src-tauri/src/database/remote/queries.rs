// Remote query functions for fetching data from Supabase
//
// These are read-only operations (SELECT) that don't fit the Syncable trait.
// Includes pattern browsing, implementation fetching, and profile lookups.

use super::common::{SupabaseClient, SyncError};
use serde::Deserialize;

// ============================================================================
// Pattern queries
// ============================================================================

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

// ============================================================================
// Profile queries
// ============================================================================

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

// ============================================================================
// Implementation queries
// ============================================================================

/// Row returned when fetching a published implementation from Supabase
#[derive(Deserialize)]
pub struct PublishedImplementationRow {
    pub id: String,
    pub uid: String,
    pub name: Option<String>,
    pub graph_json: String,
}

/// Fetch the implementation for a pattern from Supabase (by pattern_id UUID)
pub async fn fetch_implementation_by_pattern(
    client: &SupabaseClient,
    pattern_id: &str,
    access_token: &str,
) -> Result<Option<PublishedImplementationRow>, SyncError> {
    let rows: Vec<PublishedImplementationRow> = client
        .select(
            "implementations",
            &format!(
                "pattern_id=eq.{}&select=id,uid,name,graph_json&limit=1",
                pattern_id
            ),
            access_token,
        )
        .await?;
    Ok(rows.into_iter().next())
}
