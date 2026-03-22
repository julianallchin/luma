// Remote CRUD operations for implementations table

use super::common::{SupabaseClient, SyncError};
use crate::models::implementations::Implementation;
use serde::{Deserialize, Serialize};

/// Payload for upserting an implementation to Supabase
#[derive(Serialize)]
struct ImplementationPayload<'a> {
    id: &'a str,
    uid: &'a str,
    pattern_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    graph_json: &'a str,
}

/// Upsert an implementation in Supabase (idempotent).
///
/// The local UUID is sent as the cloud `id`. Uses ON CONFLICT(id) upsert.
/// The pattern_id is taken directly from the implementation (already a UUID).
pub async fn upsert_implementation(
    client: &SupabaseClient,
    implementation: &Implementation,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = implementation
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = ImplementationPayload {
        id: &implementation.id,
        uid,
        pattern_id: &implementation.pattern_id,
        name: implementation.name.as_deref(),
        graph_json: &implementation.graph_json,
    };

    client
        .upsert_no_return("implementations", &payload, "id", access_token)
        .await
}

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
