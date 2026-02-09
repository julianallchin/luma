// Remote CRUD operations for implementations table

use super::common::{SupabaseClient, SyncError};
use crate::models::implementations::Implementation;
use serde::{Deserialize, Serialize};

/// Payload for upserting an implementation to Supabase
#[derive(Serialize)]
struct ImplementationPayload<'a> {
    uid: &'a str,
    pattern_id: i64, // Cloud pattern ID (from pattern's remote_id)
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    graph_json: &'a str,
}

/// Insert or update an implementation in Supabase
///
/// Returns the cloud ID (either newly generated or existing remote_id).
///
/// # Arguments
/// * `client` - Supabase client
/// * `implementation` - The implementation to sync
/// * `pattern_remote_id` - The cloud ID of the pattern (from pattern's remote_id)
/// * `access_token` - User's access token
pub async fn upsert_implementation(
    client: &SupabaseClient,
    implementation: &Implementation,
    pattern_remote_id: i64,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = implementation
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = ImplementationPayload {
        uid,
        pattern_id: pattern_remote_id,
        name: implementation.name.as_deref(),
        graph_json: &implementation.graph_json,
    };

    match &implementation.remote_id {
        None => {
            client
                .insert("implementations", &payload, access_token)
                .await
        }
        Some(remote_id_str) => {
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;
            client
                .update("implementations", remote_id, &payload, access_token)
                .await?;
            Ok(remote_id)
        }
    }
}

/// Delete an implementation from Supabase
pub async fn delete_implementation(
    client: &SupabaseClient,
    remote_id: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    client
        .delete("implementations", remote_id, access_token)
        .await
}

/// Row returned when fetching a published implementation from Supabase
#[derive(Deserialize)]
pub struct PublishedImplementationRow {
    pub id: i64,
    pub uid: String,
    pub pattern_id: i64,
    pub name: Option<String>,
    pub graph_json: String,
}

/// Fetch a single implementation by ID from Supabase
pub async fn fetch_implementation(
    client: &SupabaseClient,
    impl_id: i64,
    access_token: &str,
) -> Result<Option<PublishedImplementationRow>, SyncError> {
    let rows: Vec<PublishedImplementationRow> = client
        .select(
            "implementations",
            &format!("id=eq.{}&select=id,uid,pattern_id,name,graph_json", impl_id),
            access_token,
        )
        .await?;
    Ok(rows.into_iter().next())
}

/// Fetch the implementation for a pattern from Supabase (by cloud pattern_id)
pub async fn fetch_implementation_by_pattern(
    client: &SupabaseClient,
    pattern_remote_id: i64,
    access_token: &str,
) -> Result<Option<PublishedImplementationRow>, SyncError> {
    let rows: Vec<PublishedImplementationRow> = client
        .select(
            "implementations",
            &format!(
                "pattern_id=eq.{}&select=id,uid,pattern_id,name,graph_json&limit=1",
                pattern_remote_id
            ),
            access_token,
        )
        .await?;
    Ok(rows.into_iter().next())
}
