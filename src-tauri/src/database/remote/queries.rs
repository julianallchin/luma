// Remote query functions for fetching data from Supabase
//
// These are read-only operations (SELECT) that don't fit the Syncable trait.
// Includes pattern browsing, implementation fetching, and profile lookups.

use super::common::{SupabaseClient, SyncError};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
    pub is_verified: bool,
    pub author_name: Option<String>,
    pub category_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Fetch all verified patterns from Supabase
pub async fn fetch_published_patterns(
    client: &SupabaseClient,
    access_token: &str,
) -> Result<Vec<RemotePatternRow>, SyncError> {
    client
        .select(
            "patterns",
            "is_verified=eq.true&select=id,uid,name,description,is_verified,author_name,category_name,created_at,updated_at",
            access_token,
        )
        .await
}

/// Fetch all patterns belonging to a specific user from Supabase (regardless of verified status)
pub async fn fetch_own_patterns(
    client: &SupabaseClient,
    uid: &str,
    access_token: &str,
) -> Result<Vec<RemotePatternRow>, SyncError> {
    client
        .select(
            "patterns",
            &format!("uid=eq.{}&select=id,uid,name,description,is_verified,author_name,category_name,created_at,updated_at", uid),
            access_token,
        )
        .await
}

/// Row returned from the search_patterns RPC.
/// Postgres returns snake_case; Tauri commands need camelCase for the frontend.
/// We use an inner struct for deserialization and convert.
#[derive(TS, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct SearchPatternRow {
    pub id: String,
    pub uid: String,
    pub name: String,
    pub description: Option<String>,
    pub is_verified: bool,
    pub author_name: Option<String>,
    pub category_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Raw row from Postgres (snake_case).
#[derive(Deserialize)]
pub(crate) struct SearchPatternRowRaw {
    pub id: String,
    pub uid: String,
    pub name: String,
    pub description: Option<String>,
    pub is_verified: bool,
    pub author_name: Option<String>,
    pub category_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<SearchPatternRowRaw> for SearchPatternRow {
    fn from(r: SearchPatternRowRaw) -> Self {
        Self {
            id: r.id,
            uid: r.uid,
            name: r.name,
            description: r.description,
            is_verified: r.is_verified,
            author_name: r.author_name,
            category_name: r.category_name,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Search for patterns used in scores (via search_patterns RPC)
pub async fn search_patterns(
    client: &SupabaseClient,
    query: &str,
    category_name: Option<&str>,
    limit: i32,
    offset: i32,
    access_token: &str,
) -> Result<Vec<SearchPatternRow>, SyncError> {
    #[derive(Serialize)]
    struct Params<'a> {
        query: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter_category_name: Option<&'a str>,
        result_limit: i32,
        result_offset: i32,
    }
    let raw: Vec<SearchPatternRowRaw> = client
        .rpc(
            "search_patterns",
            &Params {
                query,
                filter_category_name: category_name,
                result_limit: limit,
                result_offset: offset,
            },
            access_token,
        )
        .await?;
    Ok(raw.into_iter().map(SearchPatternRow::from).collect())
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
